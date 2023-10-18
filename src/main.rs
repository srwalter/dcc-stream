use std::cell::RefCell;
use std::rc::Rc;
use std::time::SystemTime;

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

    let mut queue_size = args.queue_size;

    println!("Using debug base 0x{:x}", base);

    // Make sure the CPU is powered up
    let edprsr = debug.read(base + 0x314).expect("read edprsr");
    assert!(edprsr & 1 == 1);

    // Clear OS lock
    debug.write(base + 0x300, 0).expect("write oslar");

    let dscr = debug.read(base + 0x88).expect("read dscr");
    // Enable "stall" mode
    debug.write(base + 0x88, dscr | (1 << 20)).expect("write dscr");

    let mut dup = 0;
    let mut empty = 0;
    let mut total = 0;
    let mut last = 0;
    let now = SystemTime::now();
    loop {
        for i in 0..queue_size {
            let result = debug.queue_read(base + 0x8c).expect("read dcc");
            if !result {
                println!("Limiting queue size to {}", i);
                queue_size = i;
                break;
            }
        }

        for _ in 0..queue_size {
            total += 1;

            let Ok(val) = debug.finish_read() else {
                empty += 1;
                continue;
            };

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
                println!(
                    "STATS: total: {} duplicate: {} empty: {}",
                    total, dup, empty
                );
            }
        }
    }
}
