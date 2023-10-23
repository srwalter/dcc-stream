use std::cell::RefCell;
use std::rc::Rc;
use std::time::SystemTime;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use clap::Parser;

use jtag_taps::cable;
use jtag_taps::statemachine::JtagSM;
use jtag_taps::taps::Taps;

use jtag_adi::{ArmDebugInterface, MemAP};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(short, long)]
    cable: String,
    #[arg(short, long)]
    baud: u32,
    #[arg(short, long, default_value_t = 0)]
    /// Which JTAG TAP to use
    tap_index: usize,
    #[arg(short, long, default_value_t = 1)]
    /// Which access port to use
    ap_num: u32,
    #[arg(short, long, default_value_t = 16)]
    /// Number of reads to queue per batch
    queue_size: u32,
    #[arg(long, default_value_t = false)]
    /// Ignore duplicate values
    nodups: bool,
    #[arg(long, default_value_t = false)]
    /// Show periodic statistics
    stats: bool,
    /// CPU debug base address, prefix with 0x for hexadecimal
    debug_base: String,
}

fn main() {
    let args = Args::parse();
    let cable = cable::new_from_string(&args.cable, args.baud).expect("cable");
    let jtag = JtagSM::new(cable);
    let mut taps = Taps::new(jtag);
    taps.detect();

    // IDCODE instruction
    let ir = vec![14];
    taps.select_tap(args.tap_index, &ir);
    let dr = taps.read_dr(32);
    let idcode = u32::from_le_bytes(dr.try_into().unwrap());

    // Verify ARM ID code
    if idcode != 0x4ba00477 {
        eprintln!("Warning: unexpected idcode {:x}", idcode);
    }

    let adi = Rc::new(RefCell::new(ArmDebugInterface::new(taps)));
    let mut debug = MemAP::new(adi.clone(), args.ap_num);

    let base = if args.debug_base.starts_with("0x") {
        let len = args.debug_base.len();
        u32::from_str_radix(&args.debug_base[2..len], 16).expect("failed to parse debug base")
    } else {
        str::parse(&args.debug_base).expect("failed to parse debug base")
    };

    let queue_size = args.queue_size;

    println!("Using debug base 0x{:x}", base);

    // Make sure the CPU is powered up
    let edprsr = debug.read(base + 0x314).expect("read edprsr");
    assert!(edprsr & 1 == 1);

    // Clear OS lock
    debug.write(base + 0x300, 0).expect("write oslar");

    loop {
        if let Ok(dscr) = debug.read(base + 0x88) {
            // Enable "stall" mode
            debug.write(base + 0x88, dscr | (1 << 20)).expect("write dscr");
            break;
        }
    }

    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();

    ctrlc::set_handler(move || {
        r.store(false, Ordering::SeqCst);
    }).expect("set handler");

    let mut dup = 0;
    let mut total = 0;
    let mut last = 0;
    let now = SystemTime::now();
    while running.load(Ordering::SeqCst) {
        let result = debug.read_multi(base + 0x8c, queue_size as usize, false, false).expect("read dcc");

        for val in result {
            total += 1;

            if val == last {
                dup += 1;
                last = val;
                if args.nodups {
                    continue;
                }
            }
            last = val;

            let elapsed = now.elapsed().expect("elapsed");
            println!("{}: {:x}", elapsed.as_micros(), val);

            if args.stats && total % 100 == 0 {
                eprintln!(
                    "STATS: total: {} duplicate: {} kbps: {}",
                    total, dup, (total * 32) * 1024 / elapsed.as_micros()
                );
            }
        }
    }

    if args.stats {
        let elapsed = now.elapsed().expect("elapsed");
        eprintln!(
            "STATS: total: {} duplicate: {} kbps: {}",
            total, dup, (total * 32) * 1024 / elapsed.as_micros()
        );
    }
}
