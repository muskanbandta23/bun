mod support;

use bun_engine_codegen::passes::Level;
use support::harness::{Order, Passes, campaign};

#[test]
fn full_pipeline_keeps_behaviour() {
    campaign(600, || Passes::new(Order::Pipeline(Level::Full)));
}

#[test]
fn quick_pipeline_keeps_behaviour() {
    campaign(300, || Passes::new(Order::Pipeline(Level::Quick)));
}

#[test]
fn each_pass_alone_keeps_behaviour() {
    campaign(300, || Passes::new(Order::Single));
}

#[test]
fn passes_in_any_order_keep_behaviour() {
    campaign(600, || Passes::new(Order::Random));
}
