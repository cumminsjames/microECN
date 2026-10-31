use microecn::{Outcome, replay};

#[test]
fn priority() {
    let (_, reports, _) = replay(&[
        (1, 1, (102u64 << 32) | ((-10i32) as u32 as u64)),
        (2, 2, (101u64 << 32) | ((-10i32) as u32 as u64)),
        (3, 3, (101u64 << 32) | ((-20i32) as u32 as u64)),
        (4, 4, (i32::MIN as u32 as u64) << 32 | 35),
    ]);

    assert_eq!(
        reports,
        vec![
            (1, Outcome::Ack(0)),
            (2, Outcome::Ack(1)),
            (3, Outcome::Ack(2)),
            (4, Outcome::Ack(3)),
            (2, Outcome::Fill(4, 1, 101, 10)),
            (4, Outcome::Fill(4, 3, 101, 10)),
            (3, Outcome::Fill(4, 2, 101, 20)),
            (4, Outcome::Fill(4, 3, 101, 20)),
            (1, Outcome::Fill(4, 0, 102, 5)),
            (4, Outcome::Fill(4, 3, 102, 5)),
        ]
    );
}
