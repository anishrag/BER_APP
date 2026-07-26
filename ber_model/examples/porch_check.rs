//! Dev tool: print every porch's Table 2.2 facts + verdict for a raw Ber
//! JSON (e.g. a `data_json` payload pulled from the tablet DB).
//!   cargo run -p ber_model --example porch_check -- <ber.json>
use ber_model::ber::ber_struct::Ber;
use ber_model::ber::structural::porch_verdicts;

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: porch_check <ber.json>");
    let ber: Ber = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    println!("{} — {} storeys", ber.address, ber.storeys.len());
    for (idx, facts, verdict) in porch_verdicts(&ber) {
        println!(
            "storey {idx}: key={:?} protruding={} heated={:?} door={:?} → {:?}",
            facts.key, facts.fully_protruding, facts.heated, facts.separating_door, verdict
        );
    }
}
