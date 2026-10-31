use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fs::{self, File};
use std::io::{self, Write};
use std::time::{SystemTime, UNIX_EPOCH};

// Configuration and public data types
const MAX_SIZE: u32 = 1_000_000; // max order size
const MAX_PRICE: u32 = 1_000_000_000; // max price
const MARKET: i32 = i32::MIN; // market order sentinel
const RECORD: usize = 28; // log byte length

pub type Msg = (u128, u32, u64); // timestamp (ts), client id, message
pub type Data = (bool, bool, i32, i64); // trade, side, price, quantity
pub type Feed = Vec<(u128, u32, Data)>; // ts, sequence number, market data
pub type Out = Vec<(u32, Outcome)>; // client id, order outcome

#[derive(PartialEq, Debug)]
pub enum Reason {
    InvalidSize,  // size exceeds MAX_SIZE
    InvalidPrice, // price exceeds MAX_PRICE
    Full,         // order book is full
    UnknownOrder, // order id not found
    NotOwner,     // client does not own order
}

#[derive(PartialEq, Debug)]
pub enum Outcome {
    Ack(u32),                  // assigned order id
    Fill(u128, u32, i32, i32), // ts, order id, price, size
    Cancelled(u32, i32),       // order id, remaining size
    Rejected(Reason),          // rejected order with reason
}

#[derive(Default, PartialEq, Debug)]
pub struct Book {
    counter: u32,                                    // order id counter
    orders: HashMap<u32, (bool, i32, u32)>,          // dict(id -> tuple(side, price, client))
    sides: [BTreeMap<i32, VecDeque<(u32, i32)>>; 2], // (a, b) -> tree(price -> queue(id, size))
}

// Write-ahead log
pub fn create(path: &str) -> io::Result<File> {
    File::create(path).and_then(|f| f.sync_all().map(|_| f))
}
fn append(wal: &mut File, ts: u128, client: u32, msg: u64) -> io::Result<()> {
    let record = [&ts.to_le_bytes()[..], &client.to_le_bytes(), &msg.to_le_bytes()].concat();
    wal.write_all(&record).and_then(|_| wal.sync_all())
}
fn decode(r: &[u8; RECORD]) -> Msg {
    (
        u128::from_le_bytes(r[0..16].try_into().unwrap()),
        u32::from_le_bytes(r[16..20].try_into().unwrap()),
        u64::from_le_bytes(r[20..28].try_into().unwrap()),
    )
}
pub fn load(path: &str) -> io::Result<Vec<Msg>> {
    Ok(fs::read(path)?.as_chunks::<RECORD>().0.iter().map(decode).collect())
}

// Order validation and market-data feed
fn validate(price: i32, size: i32, counter: u32) -> Result<(), Reason> {
    if size.unsigned_abs() > MAX_SIZE {
        Err(Reason::InvalidSize)
    } else if price != MARKET && price.unsigned_abs() > MAX_PRICE {
        Err(Reason::InvalidPrice)
    } else if counter == u32::MAX {
        Err(Reason::Full)
    } else {
        Ok(())
    }
}
fn publish(feed: &mut Feed, ts: u128, data: &[Data]) {
    let data = data.iter().filter(|d| !matches!(d.2, MARKET | i32::MAX));
    for (seq, &d) in (feed.len() as u32..).zip(data) {
        feed.push((ts, seq, d));
    }
}

// Order book operations
impl Book {
    fn best(&self, side: bool) -> Option<i32> {
        let book = &self.sides[side as usize]; // get best bid or ask price in O(log n)
        if side { book.last_key_value() } else { book.first_key_value() }.map(|(&p, _)| p)
    }
    fn level(&self, side: bool, price: i32) -> Data {
        let size = self.sides[side as usize]
            .get(&price)
            .map_or(0, |q| q.iter().map(|&(_, s)| s as i64).sum());
        (false, side, price, size)
    }
    fn limit(&mut self, price: i32, size: i32, client: u32) -> u32 {
        let (id, side) = (self.counter, size > 0);
        self.counter += 1;
        self.sides[side as usize].entry(price).or_default().push_back((id, size.abs()));
        self.orders.insert(id, (side, price, client));
        id
    }
    fn cancel(&mut self, id: u32) -> Option<i32> {
        let (side, price, _) = self.orders.remove(&id)?;
        let book = &mut self.sides[side as usize];
        let queue = book.get_mut(&price).unwrap();
        // walk the queue to find the order to cancel in O(n)
        let (_, lft) = queue.remove(queue.iter().position(|&(oid, _)| oid == id).unwrap()).unwrap();
        if queue.is_empty() {
            book.remove(&price);
        }
        Some(lft)
    }
    fn fill(&mut self, side: bool, price: i32, traded: i32) {
        let book = &mut self.sides[side as usize];
        let queue = book.get_mut(&price).unwrap();
        let (id, size) = queue.front_mut().unwrap(); // FIFO
        *size -= traded;
        if *size == 0 {
            self.orders.remove(id);
            queue.pop_front();
            if queue.is_empty() {
                book.remove(&price);
            }
        }
    }

    // Matching engine
    fn matching(&mut self, feed: &mut Feed, ts: u128) -> Out {
        let mut reports = vec![];
        while let (Some(bid), Some(ask)) = (self.best(true), self.best(false))
            && bid >= ask
        {
            let (bid_id, bid_size) = self.sides[1][&bid][0];
            let (ask_id, ask_size) = self.sides[0][&ask][0];
            let qty = bid_size.min(ask_size); // quantity to trade is min of sizes
            let maker = bid_id < ask_id; // adhere to price-time priority
            let price = if maker { bid } else { ask };
            let mut parties = [(bid_id, self.orders[&bid_id].2), (ask_id, self.orders[&ask_id].2)];
            parties.sort(); // older order (maker) first: sorts 2 elements in O(1)

            self.fill(true, bid, qty);
            self.fill(false, ask, qty);
            publish(
                feed,
                ts,
                &[(true, !maker, price, qty as i64), self.level(true, bid), self.level(false, ask)],
            );
            reports.extend(parties.map(|(id, c)| (c, Outcome::Fill(ts, id, price, qty))));
        }
        reports
    }
    fn apply(&mut self, feed: &mut Feed, ts: u128, client: u32, msg: u64) -> Result<Out, Reason> {
        let (size, upper) = (msg as i32, (msg >> 32) as i32); // lower and upper 32 bits
        if size == 0 {
            // cancel order, upper becomes order ID
            let id = upper as u32;
            let &(side, price, owner) = self.orders.get(&id).ok_or(Reason::UnknownOrder)?;
            if owner != client {
                return Err(Reason::NotOwner);
            }
            let left = self.cancel(id).unwrap();
            publish(feed, ts, &[self.level(side, price)]);
            return Ok(vec![(client, Outcome::Cancelled(id, left))]);
        }
        validate(upper, size, self.counter)?; // limit or market order, upper becomes price
        let market = upper == MARKET; // immediate-or-cancel
        let price = if market && size > 0 { i32::MAX } else { upper };
        let id = self.limit(price, size, client);
        publish(feed, ts, &[self.level(size > 0, price)]);
        let mut reports = vec![(client, Outcome::Ack(id))];
        reports.extend(self.matching(feed, ts));
        if market {
            // drop any unfilled remainder so it never rests
            reports.extend(self.cancel(id).map(|left| (client, Outcome::Cancelled(id, left))));
        }
        Ok(reports)
    }
    fn handle(&mut self, feed: &mut Feed, ts: u128, client: u32, msg: u64) -> Out {
        self.apply(feed, ts, client, msg).unwrap_or_else(|r| vec![(client, Outcome::Rejected(r))])
    }
}

// Public processing and replay API
pub fn receive(w: &mut File, lob: &mut Book, feed: &mut Feed, c: u32, msg: u64) -> io::Result<Out> {
    let ts = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    append(w, ts, c, msg).map(|_| lob.handle(feed, ts, c, msg))
}

pub fn replay(log: &[Msg]) -> (Book, Out, Feed) {
    let (mut lob, mut feed) = (Book::default(), Feed::new());
    let reports = log.iter().flat_map(|&(ts, c, msg)| lob.handle(&mut feed, ts, c, msg)).collect();
    (lob, reports, feed)
}
