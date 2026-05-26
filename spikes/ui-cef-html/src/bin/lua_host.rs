//! Item 4: Lua -> DOM `render(host)` ergonomics probe.
//!
//! Question being answered: when a Mote element is `render = function(host) ... end`
//! and the chrome is an HTML/CSS document in CEF, what does `host` look like, and is
//! it ergonomic vs the immediate-mode painter the wgpu/egui spikes measured?
//!
//! Approach: `host` is an mlua UserData that BUILDS AN HTML STRING (a DOM subtree).
//! The runtime would inject that string into the chrome document (innerHTML of the
//! target slot, or via CEF's message router as a DOM patch). Tokens resolve to
//! `var(--name)` so the CSS cascade — not Rust — owns the actual values.
//!
//! This builds the SAME integrity-panel permission row the other spikes built, the
//! Mote way: `host:el(tag){attrs}` opens an element, `host:text(s)` adds a text node,
//! `host:token(name)` returns the CSS var reference, `host:close()` closes.
use mlua::{Lua, Result, UserData, UserDataMethods, Value};
use std::cell::RefCell;
use std::rc::Rc;

/// Shared HTML buffer + a stack of open tags, behind the UserData.
#[derive(Clone, Default)]
struct Dom {
    html: Rc<RefCell<String>>,
    stack: Rc<RefCell<Vec<String>>>,
}

struct Host {
    dom: Dom,
}

impl UserData for Host {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        // host:el("div", { class = "perm", id = "p1" })  -> opens <div ...>
        // attrs table is optional.
        methods.add_method("el", |_, this, (tag, attrs): (String, Option<mlua::Table>)| {
            let mut open = format!("<{tag}");
            if let Some(t) = attrs {
                for pair in t.pairs::<String, Value>() {
                    let (k, v) = pair?;
                    let vs = match v {
                        Value::String(s) => s.to_str()?.to_owned(),
                        Value::Integer(i) => i.to_string(),
                        Value::Number(n) => n.to_string(),
                        _ => String::new(),
                    };
                    open.push_str(&format!(" {k}=\"{vs}\""));
                }
            }
            open.push('>');
            this.dom.html.borrow_mut().push_str(&open);
            this.dom.stack.borrow_mut().push(tag);
            Ok(())
        });

        // host:text("...")  -> text node (HTML-escaped)
        methods.add_method("text", |_, this, s: String| {
            let esc = s
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;");
            this.dom.html.borrow_mut().push_str(&esc);
            Ok(())
        });

        // host:token("color.accent") -> "var(--accent)" — the CSS cascade owns the value.
        methods.add_method("token", |_, _this, name: String| {
            let css = name.replace('.', "-").replace('_', "-");
            Ok(format!("var(--{css})"))
        });

        // host:close()  -> closes the most recently opened element.
        methods.add_method("close", |_, this, ()| {
            if let Some(tag) = this.dom.stack.borrow_mut().pop() {
                this.dom.html.borrow_mut().push_str(&format!("</{tag}>"));
            }
            Ok(())
        });
    }
}

fn main() -> Result<()> {
    let lua = Lua::new();
    let dom = Dom::default();
    let host = Host { dom: dom.clone() };
    lua.globals().set("host", host)?;

    // ---- a Mote element, as a plugin author would write it ----
    // Builds ONE integrity-panel permission row.
    let element: mlua::Function = lua
        .load(
            r#"
            return function(host, perm)
              -- a permission row inside the integrity card
              host:el("div", { class = "perm" })
                host:el("span", { class = "glyph", style = "color:" .. host:token("success") })
                  host:text("✓")
                host:close()
                host:el("code", { class = "perm-text", style = "color:" .. host:token("fg.1") })
                  host:text(perm)
                host:close()
              host:close()
            end
        "#,
        )
        .eval()?;

    // Runtime calls the element's render fn (here once; in chrome it's per dirty element).
    let _: () = element.call((
        lua.globals().get::<Value>("host")?,
        "http:fetch:https://*.1password.com/*",
    ))?;

    let html = dom.html.borrow().clone();
    println!("--- generated DOM subtree ---\n{html}\n");
    println!("balanced tags: {}", dom.stack.borrow().is_empty());

    // microbench: how expensive is the FFI per host call?
    let n = 100_000u32;
    let start = std::time::Instant::now();
    let bench: mlua::Function = lua
        .load(r#"return function(host) host:el("span") host:text("x") host:close() end"#)
        .eval()?;
    for _ in 0..n {
        let _: () = bench.call(lua.globals().get::<Value>("host")?)?;
    }
    let per = start.elapsed().as_nanos() as f64 / (n as f64 * 3.0);
    println!("FFI: {n} elements * 3 host calls in {:?} = {per:.0} ns/call", start.elapsed());

    Ok(())
}
