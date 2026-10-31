use std::io;

use microecn::{Book, Feed, Out, create, load, receive, replay};

fn main() -> io::Result<()> {
    // Start a fresh write-ahead log, order book, and market-data feed.
    let (mut wal, mut lob, mut feed) = (create("ecn.log")?, Book::default(), Feed::new());
    let mut reports: Out = Vec::new();

    // A message packs the price in its upper 32 bits and signed size in its lower 32.
    // Negative size is an ask, so Alice offers 10 units at 101.
    let alice = receive(&mut wal, &mut lob, &mut feed, 1, (101u64 << 32) | (-10i32 as u32 as u64))?;
    println!("Alice offers 10 at 101: {alice:?}");
    reports.extend(alice);

    // Positive size is a bid. Bob's 102 bid crosses Alice's 101 ask and trades 4 units.
    let bob = receive(&mut wal, &mut lob, &mut feed, 2, (102u64 << 32) | 4)?;
    println!("Bob bids 4 at 102, crossing Alice's ask: {bob:?}");
    reports.extend(bob);

    // Feed entries show book-level updates and trades in sequence.
    println!("Trade feed: {feed:?}");

    // Rebuild state from the log and verify it matches the live book and outputs.
    let log = load("ecn.log")?;
    let (replayed, replayed_reports, replayed_feed) = replay(&log);
    assert!(replayed == lob && replayed_reports == reports && replayed_feed == feed);
    println!("Replay verified: {} messages", log.len());
    Ok(())
}
