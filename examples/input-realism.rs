//! Verify the engine's input primitives emit REAL DOM events (not paste-like
//! synthetic fills). Injects listeners, drives click/type/scroll via the public
//! API, and prints the event counts + observed inter-key timing.
//!
//!   cargo run --example input-realism

use envoyage::BrowserSession;
use std::time::Duration;

const INSTRUMENT: &str = r#"
(() => {
  window.__ev = { keydown:0, keyup:0, keypress:0, input:0, mousemove:0, mousedown:0, mouseup:0, click:0, wheel:0, keyTimes:[] };
  const inp = document.createElement('input');
  inp.id = '__probe_input'; inp.type = 'text';
  inp.style.cssText = 'position:fixed;top:100px;left:100px;width:300px;height:40px;z-index:99999;font-size:20px';
  document.body.appendChild(inp);
  inp.addEventListener('keydown', e => { window.__ev.keydown++; window.__ev.keyTimes.push(Math.round(performance.now())); });
  inp.addEventListener('keyup',   () => window.__ev.keyup++);
  inp.addEventListener('keypress',() => window.__ev.keypress++);
  inp.addEventListener('input',   () => window.__ev.input++);
  document.addEventListener('mousemove', () => window.__ev.mousemove++);
  document.addEventListener('mousedown', () => window.__ev.mousedown++);
  document.addEventListener('mouseup',   () => window.__ev.mouseup++);
  document.addEventListener('click',     () => window.__ev.click++);
  document.addEventListener('wheel',     () => window.__ev.wheel++, { passive: true });
  inp.focus();
  return inp.getBoundingClientRect().x + ',' + inp.getBoundingClientRect().y;
})()
"#;

fn main() {
    let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let mut b = BrowserSession::launch(&rt, "https://example.com").expect("launch");
    std::thread::sleep(Duration::from_millis(1500));
    b.ensure_live_target().ok();
    let _ = b.eval(INSTRUMENT).expect("instrument");

    // Type into the focused input, click on it, scroll the page.
    b.type_text("Hello, world!").expect("type");
    b.click(140.0, 120.0).expect("click");
    b.scroll(400.0).expect("scroll");
    std::thread::sleep(Duration::from_millis(200));

    let ev = b.eval("JSON.stringify(window.__ev)").expect("read");
    let val = b.eval("document.getElementById('__probe_input').value").unwrap_or_default();
    println!("INPUT_VALUE {val:?}");
    println!("EVENTS {ev}");
    // Inter-key deltas — a bot that "types" via insertText shows ZERO keydowns.
    let deltas = b.eval(
        "(()=>{const t=window.__ev.keyTimes;const d=[];for(let i=1;i<t.length;i++)d.push(t[i]-t[i-1]);return JSON.stringify(d)})()",
    ).unwrap_or_default();
    println!("KEY_DELTAS_MS {deltas}");
    b.close();
}
