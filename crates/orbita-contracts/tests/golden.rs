use orbita_contracts::{InboundEvent, Reply};

#[test]
fn inbound_golden_json_stays_compatible() {
    let event: InboundEvent = serde_json::from_str(include_str!("fixtures/inbound.json"))
        .expect("golden inbound JSON must deserialize");
    event.validate().expect("golden inbound must validate");
    assert_eq!(event.turn_id, "turn-1");
    assert_eq!(event.room.as_deref(), Some("детская"));
}

#[test]
fn reply_golden_json_stays_compatible() {
    let reply: Reply = serde_json::from_str(include_str!("fixtures/reply.json"))
        .expect("golden reply JSON must deserialize");
    reply.validate().expect("golden reply must validate");
    assert_eq!(reply.blocks.len(), 2);
    assert!(reply.blocks[1].is_final);
}
