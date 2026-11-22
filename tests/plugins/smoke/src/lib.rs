//! In-game smoke tests: the agent's pump, frame counter and test plumbing.
//! `expected_*` tests fail on purpose; run them with `--filter expected_` to check that
//! failures and panics are reported correctly.

use std::sync::Mutex;
use std::time::Duration;

use f4x_sdk::console::{Arg, Param};
use f4x_sdk::{check, frames, next_pump, pumps, wait_frames, wait_pumps, wait_until};

/// What `f4xecho` last received: (count, text, reference form id).
static ECHO: Mutex<Option<(Option<i32>, Option<String>, Option<u32>)>> = Mutex::new(None);

fn form_id(form: *mut std::ffi::c_void) -> Option<u32> {
    #[cfg(windows)]
    {
        f4x_engine::Form::checked(form.cast()).map(|f| f.id().0)
    }
    #[cfg(not(windows))]
    {
        let _ = form;
        None
    }
}

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
        ctx.console_command(
            "f4xecho",
            "",
            "f4x smoke test: echo typed arguments. f4xecho [count] [text] [ref]",
            &[Param::int("count").optional(), Param::string("text").optional(), Param::reference("ref").optional()],
            |call| {
                let reference = match call.args.get(2) {
                    Some(Arg::Form(form)) => form_id(*form),
                    _ => None,
                };
                let seen = (call.int(0), call.text(1).map(str::to_owned), reference);
                *ECHO.lock().unwrap() = Some(seen.clone());
                Ok(format!("f4xecho: count={:?} text={:?} ref={:?}", seen.0, seen.1, seen.2.map(|id| format!("{id:08X}"))))
            },
        );
        ctx.test("console_command_typed_args", || async {
            *ECHO.lock().unwrap() = None;
            check(f4x_sdk::console::execute("f4xecho 3 hello player"), "console_execute failed")?;
            wait_until(|| ECHO.lock().unwrap().is_some(), Duration::from_secs(5)).await?;
            let seen = ECHO.lock().unwrap().clone().unwrap();
            check(seen == (Some(3), Some("hello".into()), Some(0x14)), format!("got {seen:?}"))?;
            #[cfg(windows)]
            {
                let log = f4x_engine::console::text().unwrap_or_default();
                check(log.contains("f4xecho: count=Some(3)"), "output not printed to the console")?;
            }
            Ok(())
        });
        ctx.test("console_command_optional_args", || async {
            *ECHO.lock().unwrap() = None;
            check(f4x_sdk::console::execute("f4xecho"), "console_execute failed")?;
            wait_until(|| ECHO.lock().unwrap().is_some(), Duration::from_secs(5)).await?;
            let seen = ECHO.lock().unwrap().clone().unwrap();
            check(seen == (None, None, None), format!("got {seen:?}"))
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
