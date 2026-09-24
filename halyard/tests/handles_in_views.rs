//! Handles as view values (docs/no-panics.md, "What an application writes"): weak and strong
//! signals, memos, mapped signals, and what `map` and `memo` give, rendered directly as
//! children, attribute, property, class, style and inner-HTML values, and as the sources of
//! `<Show>`, `<For>`, `<ForEnumerate>` and `<ShowLet>`. A handle whose source is gone renders
//! nothing.
//!
//! The server renders each position here; with `hydrate`, the same components are
//! type-checked for the browser (`hydrate_positions`).
#![cfg(any(feature = "ssr", feature = "hydrate"))]

use halyard::prelude::*;

#[derive(Clone, Debug, PartialEq)]
struct Row {
    id: u32,
    label: String,
}

/// The sources every component reads. They are made in an owner of their own, so that a
/// test can make them gone while the handles derived from them stay.
#[derive(Clone, Copy)]
struct Sources {
    count: RwSignal<i32>,
    valid: RwSignal<bool>,
    pending: RwSignal<bool>,
    shown: RwSignal<bool>,
    name: RwSignal<String>,
    maybe: RwSignal<Option<String>>,
    rows: RwSignal<Vec<Row>>,
    // these hold their source (a strong reference), so they are gone with their own owner
    second: MappedSignal<String>,
    count_signal: Signal<i32>,
}

impl Sources {
    fn new() -> Self {
        let count = RwSignal::new(2);
        let pair = RwSignal::new((1, String::from("one")));
        Self {
            count,
            valid: RwSignal::new(true),
            pending: RwSignal::new(false),
            shown: RwSignal::new(true),
            name: RwSignal::new(String::from("Ada")),
            maybe: RwSignal::new(Some(String::from("some"))),
            rows: RwSignal::new(vec![
                Row {
                    id: 1,
                    label: String::from("a"),
                },
                Row {
                    id: 2,
                    label: String::from("b"),
                },
            ]),
            second: MappedSignal::new(pair, |p| &p.1, |p| &mut p.1),
            count_signal: count.into(),
        }
    }
}

#[component]
fn Children(sources: Sources) -> impl IntoView {
    let Sources {
        count,
        name,
        second,
        count_signal,
        ..
    } = sources;
    let text = count.map(|n| n.to_string());
    let double = count.memo(|n| n * 2);
    let from_memo: Signal<i32> = double.into();
    let both = (count, name).map(|(n, s)| format!("{s}{n}"));
    view! { <p>{text}"|"{double}"|"{second}"|"{count_signal}"|"{from_memo}"|"{both}</p> }
}

#[component]
fn Attributes(sources: Sources) -> impl IntoView {
    let Sources {
        valid,
        pending,
        name,
        count,
        ..
    } = sources;
    let label = name.memo(|n| n.to_uppercase());
    view! {
        <button
            disabled=(valid, pending).map(|(v, p)| !v || *p)
            title=name.map(|n| n.clone())
            aria-label=label
            data-count=count.map(|n| n.to_string())
        >
            "Save"
        </button>
        <input prop:value=name.map(|n| n.clone()) prop:disabled=pending.memo(|p| *p) />
    }
}

#[component]
fn Classes(sources: Sources) -> impl IntoView {
    let Sources {
        shown,
        valid,
        name,
        count,
        ..
    } = sources;
    let theme = name.map(|n| n.to_lowercase());
    view! {
        <div class:collapsed=shown.map(|s| !s) class:ok=valid.memo(|v| *v) class=theme />
        <div class=sources.second class=("wide", count.map(|n| *n > 1)) />
    }
}

#[component]
fn Styles(sources: Sources) -> impl IntoView {
    let Sources { count, name, .. } = sources;
    let width = count.map(|n| format!("{}px", n * 10));
    let color =
        name.memo(|n| if n.is_empty() { "red" } else { "green" }.to_string());
    let whole = count.map(|n| format!("height: {n}em"));
    view! {
        <div style:width=width style:color=color />
        <div style=whole />
    }
}

#[component]
fn InnerHtml(sources: Sources) -> impl IntoView {
    let Sources { name, count, .. } = sources;
    view! {
        <div inner_html=name.map(|n| format!("<b>{n}</b>")) />
        <div inner_html=count.memo(|n| format!("<i>{n}</i>")) />
    }
}

#[component]
fn ViewSources(sources: Sources) -> impl IntoView {
    let Sources {
        shown,
        maybe,
        rows,
        second,
        ..
    } = sources;
    view! {
        <Show when=shown.map(|s| *s) fallback=|| "hidden">"shown"</Show>
        <Show when=shown.memo(|s| !s) fallback=|| "not-hidden">"hidden"</Show>
        <ul>
            <For each=rows.map(|r| r.clone()) key=|r| r.id let:row>
                <li>{row.label}</li>
            </For>
        </ul>
        <ol>
            <ForEnumerate each=rows.memo(|r| r.clone()) key=|r| r.id let(index, row)>
                <li>{index}"."{row.label}</li>
            </ForEnumerate>
        </ol>
        <ShowLet some=maybe.map(|m| m.clone()) let:value fallback=|| "none">
            {value}
        </ShowLet>
        <ShowLet some=maybe.memo(|m| m.as_ref().map(|m| m.len())) let:len>
            {len}
        </ShowLet>
        <ShowLet some=second.map(|s| Some(s.clone())) let:s>
            {s}
        </ShowLet>
    }
}

/// A test's owner (arena), and the sources in a child owner of it.
#[cfg(feature = "ssr")]
fn setup() -> (Owner, Owner, Sources) {
    let owner = Owner::new();
    owner.set();
    let child = owner.child();
    let sources = child.with(Sources::new);
    (owner, child, sources)
}

#[cfg(feature = "ssr")]
mod ssr {
    use super::*;

    /// Renders each case twice: with its sources, and after they are gone (the handles
    /// derived from them are made in the test's owner, which is still there).
    fn render(view: fn(Sources) -> AnyView) -> (String, String) {
        let (_owner, child, sources) = setup();
        let live = view(sources).to_html();
        let (_owner, child_gone, sources) = setup();
        let derived = view(sources);
        child_gone.cleanup();
        let gone = derived.to_html();
        drop(child);
        (live, gone)
    }

    #[test]
    fn children() {
        let (live, gone) =
            render(|sources| view! { <Children sources=sources /> }.into_any());
        assert_eq!(
            live,
            "<p>2<!>|<!>4<!>|<!>one<!>|<!>2<!>|<!>4<!>|<!>Ada2</p>"
        );
        assert_eq!(gone, "<p><!>|<!>|<!>|<!>|<!>|<!></p>");
    }

    #[test]
    fn attributes_and_properties() {
        let (live, gone) = render(|sources| {
            view! { <Attributes sources=sources /> }.into_any()
        });
        assert_eq!(
            live,
            "<button title=\"Ada\" aria-label=\"ADA\" \
             data-count=\"2\">Save</button><input>"
        );
        assert_eq!(gone, "<button>Save</button><input>");

        let (_owner, _child, sources) = setup();
        sources.pending.set(true);
        let html = view! { <Attributes sources=sources /> }.to_html();
        assert!(html.starts_with("<button disabled "), "{html}");
    }

    #[test]
    fn classes() {
        let (live, gone) =
            render(|sources| view! { <Classes sources=sources /> }.into_any());
        // a toggle that is off leaves a space, as with any `class:` toggle
        assert_eq!(
            live,
            "<div class=\"ada  ok\"></div><div class=\"one wide\"></div>"
        );
        assert_eq!(gone, "<div class=\"\"></div><div class=\"\"></div>");

        let (_owner, _child, sources) = setup();
        sources.shown.set(false);
        sources.count.set(0);
        let html = view! { <Classes sources=sources /> }.to_html();
        assert_eq!(
            html,
            "<div class=\"ada collapsed ok\"></div><div class=\"one\"></div>"
        );
    }

    #[test]
    fn styles() {
        let (live, gone) =
            render(|sources| view! { <Styles sources=sources /> }.into_any());
        assert_eq!(
            live,
            "<div style=\"width:20px;color:green;\"></div><div style=\"height: \
             2em;\"></div>"
        );
        assert_eq!(gone, "<div></div><div></div>");
    }

    #[test]
    fn inner_html() {
        let (live, gone) = render(|sources| {
            view! { <InnerHtml sources=sources /> }.into_any()
        });
        assert_eq!(live, "<div><b>Ada</b></div><div><i>2</i></div>");
        assert_eq!(gone, "<div></div><div></div>");
    }

    #[test]
    fn show_for_show_let() {
        let (live, gone) = render(|sources| {
            view! { <ViewSources sources=sources /> }.into_any()
        });
        assert_eq!(
            live,
            "shown<!>not-hidden<ul><li>a</li><li>b</li><!></ul><ol><li>0<!>.<!>a</\
             li><li>1<!>.<!>b</li><!></ol>some<!>4<!>one"
        );
        assert_eq!(gone, "<!><!><ul><!></ul><ol><!></ol><!><!><!>");
    }

    /// A strong handle renders in the same positions (its value is never gone).
    #[test]
    fn strong_handles() {
        let _owner = Owner::new();
        _owner.set();
        let name = ArcRwSignal::new(String::from("Ada"));
        let pair = ArcRwSignal::new((1u32, String::from("one")));
        let second = ArcMappedSignal::new(pair, |p| &p.1, |p| &mut p.1);
        let signal: ArcSignal<String> = name.clone().into();
        let memo = ArcMemo::new({
            let name = name.clone();
            move |_| name.get().len()
        });
        let shown = ArcMemo::new(move |_| memo.get() > 1);
        let html = view! {
            <p title=second.clone() class=signal.clone() style:color=second.clone()>
                {second.clone()}"|"{signal.clone()}
            </p>
            <div inner_html=second.clone() />
            <Show when=shown.clone()>"long"</Show>
        }
        .to_html();
        assert_eq!(
            html,
            "<p title=\"one\" class=\"Ada\" \
             style=\"color:one;\">one<!>|<!>Ada</p><div>one</div>long"
        );
    }
}

/// With `hydrate`, the same views are built for the browser: this only needs to compile.
#[cfg(feature = "hydrate")]
#[allow(dead_code)]
fn hydrate_positions() {
    halyard::mount::hydrate_body(|| {
        let sources = Sources::new();
        view! {
            <Children sources=sources />
            <Attributes sources=sources />
            <Classes sources=sources />
            <Styles sources=sources />
            <InnerHtml sources=sources />
            <ViewSources sources=sources />
        }
    });
}
