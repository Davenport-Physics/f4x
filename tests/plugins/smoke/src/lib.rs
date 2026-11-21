//! In-game smoke tests: the agent's pump, frame counter and test plumbing.
//! `expected_*` tests fail on purpose; run them with `--filter expected_` to check that
//! failures and panics are reported correctly.

use std::time::Duration;

use f4x_sdk::{check, frames, next_pump, pumps, wait_frames, wait_pumps, wait_until};

f4x_sdk::plugin! {
    name: "f4x_smoke_tests",
    load: |ctx| {
        ctx.test("pump_advances", || async {
            let start = pumps();
            wait_pumps(5).await;
            check(pumps() >= start + 5, format!("pumps {start} -> {}", pumps()))
        });
        ctx.test("frames_advance", || async {
            let start = frames();
            wait_until(|| frames() >= start + 3, Duration::from_secs(10)).await?;
            wait_frames(1).await;
            check(frames() > start, "no frames presented")
        });
        ctx.test("host_is_known", || async {
            next_pump().await;
            check(matches!(f4x_sdk::host(), "f4x" | "f4se"), format!("host = {:?}", f4x_sdk::host()))
        });
        ctx.test("log_from_test", || async {
            f4x_sdk::log::info!("hello from an in-game test");
            Ok(())
        });
        ctx.test("expected_failure", || async {
            next_pump().await;
            check(false, "this test fails on purpose")
        });
        ctx.test("expected_panic", || async {
            next_pump().await;
            panic!("this test panics on purpose");
        });
        Ok(())
    },
}
