//! The dashboard, embedded in the binary.
//!
//! Compiling the UI in means the portal is still a single file to deploy, which
//! matters more here than a build pipeline would: this is a thing people run on
//! a box in a cupboard, not a service with a CDN in front of it.

use axum::response::Html;
use axum::routing::get;
use axum::Router;

pub fn routes() -> Router {
    Router::new()
        .route("/", get(|| async { Html(INDEX) }))
        // Browsers and monitoring tools ask for this whether or not the page
        // declares an icon, and a 404 on every visit is noise in the log.
        .route("/favicon.ico", get(favicon))
        .route("/favicon.svg", get(favicon))
}

/// The same mark the page declares inline, served for anything that asks for
/// it by path. Vector, so one asset covers every size, and no binary file has
/// to ship beside a single-binary portal.
async fn favicon() -> impl axum::response::IntoResponse {
    (
        [
            (axum::http::header::CONTENT_TYPE, "image/svg+xml"),
            (axum::http::header::CACHE_CONTROL, "public, max-age=86400"),
        ],
        FAVICON,
    )
}

const FAVICON: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64">
  <rect width="64" height="64" rx="12" fill="#161a20"/>
  <rect x="10" y="18" width="44" height="28" rx="4" fill="none" stroke="#58a6ff" stroke-width="4"/>
  <circle cx="22" cy="32" r="4" fill="#3fb950"/>
  <circle cx="32" cy="32" r="4" fill="#3fb950"/>
  <circle cx="42" cy="32" r="4" fill="#d29922"/>
</svg>"##;

const INDEX: &str = r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>PatchPanel</title>
<link rel="icon" href="data:image/svg+xml,<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 64 64'><rect width='64' height='64' rx='12' fill='%23161a20'/><rect x='10' y='18' width='44' height='28' rx='4' fill='none' stroke='%2358a6ff' stroke-width='4'/><circle cx='22' cy='32' r='4' fill='%233fb950'/><circle cx='32' cy='32' r='4' fill='%233fb950'/><circle cx='42' cy='32' r='4' fill='%23d29922'/></svg>">
<style>
  :root {
    color-scheme: light dark;
    --bg: #f6f7f9;
    --panel: #ffffff;
    --line: #dfe3e8;
    --ink: #16191d;
    --muted: #6b7280;
    --accent: #2563eb;
    --ok: #15803d;
    --warn: #b45309;
    --bad: #b91c1c;
    --mono: ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, monospace;
  }
  @media (prefers-color-scheme: dark) {
    :root {
      --bg: #0f1115; --panel: #171a21; --line: #272b34;
      --ink: #e6e8ec; --muted: #9aa3af; --accent: #60a5fa;
      --ok: #4ade80; --warn: #fbbf24; --bad: #f87171;
    }
  }
  * { box-sizing: border-box; }
  body {
    margin: 0; background: var(--bg); color: var(--ink);
    font: 14px/1.5 system-ui, -apple-system, "Segoe UI", Roboto, sans-serif;
  }
  header {
    display: flex; align-items: baseline; gap: 16px; flex-wrap: wrap;
    padding: 14px 20px; border-bottom: 1px solid var(--line); background: var(--panel);
    position: sticky; top: 0; z-index: 10;
  }
  header h1 { font-size: 16px; margin: 0; letter-spacing: -0.01em; }
  header .rev { color: var(--muted); font-family: var(--mono); font-size: 12px; }
  nav { margin-left: auto; display: flex; gap: 4px; }
  nav button {
    background: none; border: 1px solid transparent; color: var(--muted);
    padding: 5px 11px; border-radius: 6px; cursor: pointer; font: inherit;
  }
  nav button.active { color: var(--ink); border-color: var(--line); background: var(--bg); }
  /* How many need attention, without having to open the tab to find out. */
  .badge { display: inline-block; margin-left: 6px; padding: 0 6px; border-radius: 10px;
    font-size: 11px; font-family: var(--mono); line-height: 17px;
    background: var(--warn); color: var(--panel); }
  .badge.bad { background: var(--bad); }
  /* Nothing to do is worth saying, not hiding: a zero in the same place the
     number lives reads at a glance as "checked, and fine". */
  .badge.ok { background: var(--ok); }
  main { padding: 20px; max-width: 1400px; margin: 0 auto; }
  section { display: none; }
  section.active { display: block; }

  /* Eleven of these wrapped onto two rows and pushed the fleet table below
     the fold. They are a glance, not a dashboard: number beside label, sized
     to the content, wrapping only when the window genuinely cannot hold them. */
  .tiles { display: flex; flex-wrap: wrap; gap: 8px; margin-bottom: 16px; }
  .tile { background: var(--panel); border: 1px solid var(--line); border-radius: 8px;
    padding: 6px 10px; display: flex; align-items: baseline; gap: 6px; }
  .tile .n { font-size: 17px; font-weight: 600; line-height: 1.2; font-variant-numeric: tabular-nums; }
  .tile .l { color: var(--muted); font-size: 11.5px; white-space: nowrap; }
  .tile.warn .n { color: var(--warn); }
  .tile.bad .n { color: var(--bad); }

  .card { background: var(--panel); border: 1px solid var(--line); border-radius: 10px; overflow: hidden; margin-bottom: 20px; }
  .card > h2 { font-size: 13px; margin: 0; padding: 11px 14px; border-bottom: 1px solid var(--line); color: var(--muted); font-weight: 600; text-transform: uppercase; letter-spacing: 0.04em; }
  .scroll { overflow-x: auto; }
  /* Long tables get a viewport of their own rather than pushing the rest of
     the page out of reach - `bit` has 111 pending updates. */
  .scrolly { max-height: 420px; overflow: auto; }
  .scrolly thead th { position: sticky; top: 0; z-index: 1; background: var(--panel); }
  table { border-collapse: collapse; width: 100%; font-size: 13px; }
  th, td { text-align: left; padding: 7px 10px; border-bottom: 1px solid var(--line); white-space: nowrap; vertical-align: top; }
  td .msg { font-size: 11px; }
  th { color: var(--muted); font-weight: 600; font-size: 12px; }
  tr:last-child td { border-bottom: none; }
  tbody tr:hover { background: var(--bg); }
  td.mono, .mono { font-family: var(--mono); font-size: 12px; }

  .dot { display: inline-block; width: 8px; height: 8px; border-radius: 50%; margin-right: 6px; vertical-align: 1px; }
  .dot.on { background: var(--ok); }
  .dot.off { background: var(--muted); }
  .dot.bad { background: var(--bad); }
  .pill { display: inline-block; padding: 1px 7px; border-radius: 20px; font-size: 11px; border: 1px solid var(--line); color: var(--muted); }
  .pill.bad { color: var(--bad); border-color: var(--bad); }
  .pill.warn { color: var(--warn); border-color: var(--warn); }
  .pill.ok { color: var(--ok); border-color: var(--ok); }
  .pill.busy { color: var(--accent); border-color: var(--accent); }
  .cmdid { cursor: pointer; border: 1px solid var(--line); border-radius: 4px; padding: 0 5px; margin-left: 6px; }
  .cmdid:hover { color: var(--accent); border-color: var(--accent); }
  .pill.busy::before {
    content: ""; display: inline-block; width: 7px; height: 7px; margin-right: 5px;
    border-radius: 50%; background: var(--accent); animation: pulse 1.1s ease-in-out infinite;
  }
  @keyframes pulse { 0%,100% { opacity: 1 } 50% { opacity: .25 } }

  button.act {
    font: inherit; font-size: 12px; padding: 3px 8px; margin-right: 3px;
    background: var(--bg); color: var(--ink);
    border: 1px solid var(--line); border-radius: 6px; cursor: pointer;
  }
  button.act:hover { border-color: var(--accent); color: var(--accent); }
  button.act:disabled { opacity: .4; cursor: default; }
  button.primary { background: var(--accent); color: #fff; border-color: var(--accent); }

  textarea {
    width: 100%; min-height: 460px; padding: 12px 14px; border: none; resize: vertical;
    background: var(--panel); color: var(--ink); font-family: var(--mono); font-size: 12.5px; line-height: 1.55;
  }
  textarea:focus { outline: none; }
  .bar { display: flex; gap: 8px; align-items: center; padding: 10px 14px; border-top: 1px solid var(--line); }
  .msg { font-size: 12px; color: var(--muted); }
  .msg.bad { color: var(--bad); }
  .msg.ok { color: var(--ok); }
  .empty { padding: 28px 14px; text-align: center; color: var(--muted); font-size: 13px; }
  details summary { cursor: pointer; color: var(--muted); font-size: 12px; }
  pre { margin: 8px 0 0; padding: 10px; background: var(--bg); border-radius: 6px; font-family: var(--mono); font-size: 11.5px; white-space: pre-wrap; word-break: break-word; max-height: 320px; overflow: auto; }

  .step { padding: 14px; border-bottom: 1px solid var(--line); }
  .step:last-child { border-bottom: none; }
  .step h3 { margin: 0 0 8px; font-size: 13px; font-weight: 600; }
  .step p { margin: 0 0 10px; color: var(--muted); font-size: 12.5px; }
  .cmd { position: relative; }
  .cmd pre { margin: 0; max-height: none; }
  .cmd button {
    position: absolute; top: 6px; right: 6px; font: inherit; font-size: 11px;
    padding: 3px 9px; background: var(--panel); color: var(--muted);
    border: 1px solid var(--line); border-radius: 5px; cursor: pointer;
  }
  .cmd button:hover { color: var(--accent); border-color: var(--accent); }
  .field { display: flex; gap: 8px; align-items: center; margin-bottom: 12px; }
  .field label { font-size: 12.5px; color: var(--muted); }
  .field input {
    padding: 5px 9px; border: 1px solid var(--line); border-radius: 6px;
    background: var(--bg); color: var(--ink); font-family: var(--mono); font-size: 12.5px;
  }
  .note { padding: 10px 12px; border-radius: 8px; font-size: 12.5px; border: 1px solid var(--line); background: var(--bg); margin-bottom: 12px; }

  .diff { font-family: var(--mono); font-size: 12px; line-height: 1.5; border: 1px solid var(--line);
    border-radius: 8px; overflow: auto; max-height: 320px; background: var(--bg); margin: 8px 0; }
  .diff div { padding: 1px 10px; white-space: pre-wrap; word-break: break-word; }
  .diff .del { background: color-mix(in srgb, var(--bad) 16%, transparent); color: var(--bad); }
  .diff .add { background: color-mix(in srgb, var(--ok) 16%, transparent); color: var(--ok); }
  .diff .same { color: var(--muted); }
  .status { font-size: 12.5px; margin-left: 8px; }
  .status.run { color: var(--accent); }
  .status.ok { color: var(--ok); }
  .status.bad { color: var(--bad); }
  #edit-banner { position: sticky; top: 56px; z-index: 5; margin-bottom: 12px; padding: 8px 12px; border-radius: 8px;
    border: 1px solid var(--accent); color: var(--accent); background: var(--panel); font-size: 12.5px; }
  /* Editable in place, but not shouting about it until you are in it. */
  .namefld { background: none; border: 1px solid transparent; border-radius: 6px;
    color: var(--ink); font: inherit; padding: 3px 6px; width: 100%; max-width: 240px; }
  .namefld:hover { border-color: var(--line); }
  .namefld:focus { border-color: var(--accent); background: var(--bg); outline: none; }
  .namefld::placeholder { color: var(--muted); font-style: italic; }
  /* Two weeks at a glance. Days are equal width so the shape of a schedule -
     nightly, weekly, nothing - is visible without reading any of it. */
  .calwrap { overflow-x: auto; }
  .calhead, .cal { display: grid; grid-template-columns: repeat(7, minmax(74px, 1fr)); gap: 4px; }
  .calhead { margin: 10px 14px 4px; }
  .calhead div { font-size: 10.5px; letter-spacing: .06em; text-transform: uppercase;
    color: var(--muted); }
  .cal { margin: 0 14px 12px; }
  .cal .day { border: 1px solid var(--line); border-radius: 7px; padding: 5px 6px 8px;
    min-height: 96px; background: var(--bg); display: flex; flex-direction: column; gap: 3px; }
  .cal .day.out { opacity: .3; }
  .cal .day.today { border-color: var(--accent); box-shadow: inset 0 0 0 1px var(--accent); }
  .cal .d { font-family: var(--mono); font-size: 10px; color: var(--muted);
    display: flex; justify-content: space-between; align-items: baseline; }
  .cal .d b { color: var(--ink); font-size: 12px; }
  .cal .day.today .d b { color: var(--accent); }
  /* A repeating schedule has exactly one thing to say, and three pixels is
     enough to say it. Spelling out "05:00 fun" on fourteen identical cells
     spends a grid on nine characters. */
  .cal .tick { height: 3px; border-radius: 2px; background: var(--accent); opacity: .4;
    flex: none; }
  .cal .ev { border-left: 2px solid var(--muted); padding-left: 5px; font-size: 10.5px;
    line-height: 1.3; overflow: hidden; flex: none; white-space: normal; }
  .cal .ev .t { font-family: var(--mono); font-size: 10px; color: var(--muted); }
  .cal .ev .o { color: var(--muted); display: block; }
  .cal .ev.ran { border-left-color: var(--ink); }
  .cal .ev.nothing { border-left-color: var(--line); color: var(--muted); }
  .cal .ev.failed { border-left-color: var(--bad); }
  .cal .ev.manual { border-left-color: var(--accent); }
  .cal .ev.job { border-left-style: dotted; }
  .cal .ev.next { border-left-style: dashed; border-left-color: var(--accent); }
  .callegend { display: flex; flex-wrap: wrap; gap: 6px 15px; margin: 0 14px 12px;
    font-size: 11.5px; color: var(--muted); }
  .callegend span { display: inline-flex; align-items: center; gap: 6px; }
  .callegend em { width: 13px; border-top: 2px solid var(--muted); font-style: normal; }
  .callegend em.ink { border-color: var(--ink); }
  .callegend em.bad { border-color: var(--bad); }
  .callegend em.acc { border-color: var(--accent); }
  .callegend em.next { border-top-style: dashed; border-color: var(--accent); }
  /* The attention list. The left rule is the only colour on a row, and it
     carries the whole scale: red means the numbers on this page are not known
     to be true, amber means a person must act and nothing else will, grey
     means it is waiting on you but nothing is degrading meanwhile. */
  /* One machine with a long second line used to widen the Machine column for
     every row and throw the whole table into horizontal scroll. The cap is
     what stops one row from degrading twelve others. */
  td.machine { max-width: 320px; }
  td.machine .msg { overflow: hidden; text-overflow: ellipsis; }
  /* Same cap for the same reason: one embedded box answering on six ports with
     a version string on each is wider than every other column put together. */
  td.svc { max-width: 300px; }
  td.what { max-width: 260px; overflow: hidden; text-overflow: ellipsis; }

  .live-controls { display: flex; gap: 8px; align-items: center; flex-wrap: wrap;
    padding: 0 14px 10px; }
  .live-controls input { flex: 1 1 200px; min-width: 140px; }
  /* Fixed height, own scrollbar: the feed must not make the page grow without
     bound as lines arrive, and following the tail means scrolling this and not
     the document. */
  /* `overflow-x: hidden`, not auto: a single long line - a firewall's filterlog
     entries are hundreds of characters - brings a horizontal scrollbar into
     existence, which takes about fifteen pixels off the visible height and shifts
     every line on screen. That is a second, smaller jump, and it fires whenever
     the widest line changes. Long messages wrap instead, which is what somebody
     reading a log wants anyway. */
  .feed { height: 420px; overflow-y: auto; overflow-x: hidden; margin: 0 14px 14px;
    border: 1px solid var(--line); border-radius: 6px; background: var(--bg);
    font-family: var(--mono); font-size: 12px; line-height: 1.5; }
  /* The row wraps; the message inside it keeps its own spacing. `pre` on the row
     would stop it wrapping at all and put the scrollbar back. */
  .feed .row { display: flex; gap: 8px; padding: 1px 8px; align-items: flex-start;
    border-bottom: 1px solid color-mix(in srgb, var(--line) 35%, transparent); }
  .feed .row:hover { background: color-mix(in srgb, var(--accent) 8%, transparent); }
  .feed .t { color: var(--muted); white-space: nowrap; }
  /* One column so machine names line up and the eye can run down them, which is
     the whole point of a merged feed. */
  .feed .w { color: var(--accent); min-width: 110px; flex: 0 0 auto;
    white-space: nowrap; }
  .feed .g { color: var(--muted); min-width: 70px; flex: 0 0 auto;
    white-space: nowrap; overflow: hidden; text-overflow: ellipsis; max-width: 130px; }
  /* Takes the rest, and may use several lines of it. `min-width: 0` because a flex
     item will otherwise refuse to shrink below its longest word and push the row
     wider than the feed. */
  .feed .m { color: var(--ink); white-space: pre-wrap; word-break: break-word;
    flex: 1 1 auto; min-width: 0; }
  .feed .row.warn .m { color: var(--warn); }
  .feed .row.err .m { color: var(--bad); }

/* A line materialises as it lands. Only opacity, and deliberately brief.
     The movement belongs to the feed growing a row at a time underneath the
     viewport (see dripLive) - this just stops each row appearing as a hard edge.
     Opacity and transform are the two things that animate without forcing layout,
     which matters here because the drip keeps a few dozen of these in flight at
     once; animating height instead would be a layout pass per row per frame. */
  @keyframes live-in {
    from { opacity: 0; }
    to   { opacity: 1; }
  }
  .feed .row.fresh { animation: live-in 150ms ease-out both; }
  /* Somebody who has asked for less movement is reading a log, of all things,
     precisely to find something - so this one is not decoration to insist on. */
  @media (prefers-reduced-motion: reduce) {
    .feed .row.fresh { animation: none; }
  }

  /* Full screen. The feed takes whatever is left after the heading and controls,
     rather than keeping its fixed height and leaving the rest of the screen
     empty, which is the whole reason to ask for full screen. */
  #live-card:fullscreen {
    display: flex; flex-direction: column;
    background: var(--panel); border-radius: 0; margin: 0;
    max-height: 100vh; overflow: hidden;
  }
  #live-card:fullscreen #live-blurb { display: none; }
  #live-card:fullscreen .feed {
    flex: 1 1 auto; height: auto; min-height: 0;
    border-radius: 0; border-left: 0; border-right: 0; margin: 0;
  }
  /* A hair larger, since the point of filling the screen is to read it from
     further away than a card in a page. */
  #live-card:fullscreen .feed .row { font-size: 12.5px; padding: 2px 12px; }
  td.svc div { overflow: hidden; text-overflow: ellipsis; }
  /* Most machines have security updates, so a pill there discriminates
     nothing - the shape is identical down the column. Colour on the numeral
     does the same work and leaves the row's one pill for its verdict. */
  .sec { color: var(--bad); }
  .of { color: var(--muted); }
  .need { display: grid; grid-template-columns: 3px minmax(0,1fr) auto;
    align-items: center; gap: 0 14px; padding: 9px 14px 9px 0;
    border-bottom: 1px solid var(--line); }
  .need:last-child { border-bottom: 0; }
  .need .rule { align-self: stretch; background: var(--muted); border-radius: 0 2px 2px 0; }
  .need.t1 .rule, .need.t2 .rule { background: var(--bad); }
  .need.t3 .rule { background: var(--warn); }
  .need .what { min-width: 0; }
  .need .tail { color: var(--muted); font-size: 12.5px; margin-top: 1px; }
  /* The count doubles as the control: it says how many, and opens them. */
  .disc { font: inherit; font-size: 11.5px; font-family: var(--mono); margin-left: 6px;
    padding: 0 6px; border-radius: 10px; border: 1px solid var(--line);
    background: transparent; color: var(--muted); cursor: pointer; }
  .disc:hover { border-color: var(--accent); color: var(--accent); }
  .who { margin-top: 6px; border-left: 1px solid var(--line); padding-left: 11px; }
  .who > div { display: grid; grid-template-columns: minmax(120px,1fr) minmax(120px,1fr) 2fr;
    gap: 10px; font-size: 12px; color: var(--muted); padding: 2px 0; }
  .who b { font-family: var(--mono); font-size: 12px; font-weight: 500; color: var(--ink); }
  .proof { padding: 16px 14px; color: var(--muted); font-size: 13.5px; }
  .proof b { color: var(--ok); font-weight: 600; }
  h2 .sub { font-weight: 400; color: var(--muted); margin-left: 9px; font-size: 12px; }
  /* Eleven tiles, replaced by one sentence. Every one of them was either a
     number that also appeared in the table below it, or a number that should
     have been a row on the list above. */
  .gist { display: flex; flex-wrap: wrap; gap: 3px 9px; align-items: baseline;
    padding: 10px 14px; margin-bottom: 14px; font-size: 13.5px; color: var(--muted);
    background: var(--panel); border: 1px solid var(--line); border-radius: 9px; }
  .gist b { color: var(--ink); font-weight: 600; font-variant-numeric: tabular-nums; }
  .gist span:not(:last-child)::after { content: " \00b7"; opacity: .5; }
  /* What the jobs know, as a board rather than a list. These are a handful
     of values somebody checks at a glance - an address, a filename, a date -
     so they get room and alignment instead of being crammed onto a row. */
  .board { display: grid; grid-template-columns: repeat(auto-fit, minmax(300px, 1fr));
    gap: 12px; margin-bottom: 14px; }
  .panel { background: var(--panel); border: 1px solid var(--line); border-radius: 9px;
    overflow: hidden; }
  .panel > h3 { margin: 0; padding: 9px 13px; font-size: 12px; font-weight: 600;
    letter-spacing: .04em; text-transform: uppercase; color: var(--muted);
    border-bottom: 1px solid var(--line); display: flex; gap: 8px; align-items: baseline; }
  .panel > h3 .when { margin-left: auto; text-transform: none; letter-spacing: 0;
    font-weight: 400; font-size: 11.5px; }
  .reading { display: grid; grid-template-columns: minmax(70px, auto) 1fr auto;
    gap: 3px 12px; align-items: baseline; padding: 8px 13px;
    border-bottom: 1px solid var(--line); }
  .reading:last-child { border-bottom: 0; }
  .reading .k { color: var(--muted); font-size: 12.5px; }
  /* The value is the thing being read, so it gets the weight and the
     tabular figures; everything around it stays quiet. */
  .reading .v { font-family: var(--mono); font-size: 13px; color: var(--ink);
    font-variant-numeric: tabular-nums; overflow: hidden; text-overflow: ellipsis;
    white-space: nowrap; }
  .reading .since { font-size: 11.5px; color: var(--muted); white-space: nowrap; }
  /* Something that moved in the last day is usually why you opened the page. */
  .reading.fresh .since { color: var(--accent); }
  tr.clicky { cursor: pointer; }
  tr.clicky.open td { background: color-mix(in srgb, var(--accent) 12%, transparent); }
  /* The expansion is one cell spanning the table, so it must not inherit the
     nowrap that keeps the columns tidy. */
  tr.detail > td { white-space: normal; background: var(--bg);
    border-bottom: 2px solid var(--accent); }
  tr.clicky:hover td { background: color-mix(in srgb, var(--accent) 8%, transparent); }
  .jobgroup { display: flex; align-items: baseline; gap: 10px; padding: 9px 14px 5px;
    border-bottom: 1px solid var(--line); background: var(--bg); }
  .jobgroup b { font-size: 12px; letter-spacing: .04em; text-transform: uppercase; }
  .job { display: grid; grid-template-columns: minmax(0,1fr) auto; gap: 4px 14px;
    padding: 10px 14px; border-bottom: 1px solid var(--line); align-items: start; }
  .job:last-of-type { border-bottom: 0; }
  .job .nm { font-family: var(--mono); font-size: 13px; }
  .job .facts { display: flex; flex-wrap: wrap; gap: 4px 14px; margin-top: 4px; }
  .job .facts b { font-family: var(--mono); font-weight: 500; }
  .job .when { text-align: right; white-space: nowrap; }
  .devgrid { display: grid; gap: 12px; margin: 12px 14px;
    grid-template-columns: repeat(auto-fill, minmax(330px, 1fr)); }
  .dev { border: 1px solid var(--line); border-radius: 10px; padding: 12px 14px;
    background: color-mix(in srgb, var(--panel) 55%, transparent); }
  .dev.bad { border-color: var(--bad); }
  .dev.warn { border-color: var(--warn); }
  .dev h3 { margin: 0 0 2px; font-size: 15px; display: flex; align-items: center; gap: 8px; }
  .dev .ver { font-family: var(--mono); font-size: 18px; margin: 8px 0 2px; }
  /* A remembered reading is still worth showing - it is the last thing that
     was true - but it must not look like a fresh one. */
  .dev .ver.stale { color: var(--muted); }
  .dev .foot { display: flex; gap: 8px; align-items: center; flex-wrap: wrap;
    margin-top: 10px; padding-top: 10px; border-top: 1px solid var(--line); }
  .fld { padding: 7px 10px; border: 1px solid var(--line); border-radius: 6px;
    background: var(--bg); color: var(--ink); font: inherit; }
  .back { display: inline-block; margin-bottom: 14px; color: var(--accent); cursor: pointer; font-size: 13px; }
  .back:hover { text-decoration: underline; }
  /* The per-machine page carries five very different kinds of information.
     Stacking them made a page you had to scroll past to reach anything, so
     each is its own pane behind a tab. */
  /* Reading a machine page means scrolling, and the way back was at the top
     of it. The header, the name and the tabs stay put. */
  .machine-head { position: sticky; top: 51px; z-index: 6; background: var(--bg);
    padding-top: 10px; margin-bottom: 18px;
    box-shadow: 0 6px 12px -12px rgba(0, 0, 0, .8); }
  .machine-head .hdr { margin-bottom: 10px; }
  .subnav { display: flex; gap: 4px; flex-wrap: wrap; border-bottom: 1px solid var(--line);
    margin: 0; padding-bottom: 8px; background: var(--bg); }
  .subnav button {
    background: none; border: 1px solid transparent; color: var(--muted);
    padding: 6px 12px; border-radius: 6px; cursor: pointer; font: inherit;
  }
  .subnav button:hover { color: var(--ink); }
  .subnav button.active { color: var(--ink); border-color: var(--line); background: var(--panel); }
  .subnav .n { font-family: var(--mono); font-size: 11.5px; color: var(--muted); margin-left: 5px; }
  .subnav button.active .n { color: var(--accent); }
  .subnav .n.warn { color: var(--warn); }
  .hdr { display: flex; align-items: baseline; gap: 12px; flex-wrap: wrap; margin-bottom: 16px; }
  .hdr h2 { margin: 0; font-size: 20px; letter-spacing: -0.01em; }
  .grid2 { display: grid; gap: 20px; grid-template-columns: repeat(auto-fit, minmax(320px, 1fr)); align-items: start; }
  dl.kv { margin: 0; padding: 4px 0; }
  dl.kv > div { display: flex; gap: 12px; padding: 7px 14px; border-bottom: 1px solid var(--line); }
  dl.kv > div:last-child { border-bottom: none; }
  dl.kv dt { flex: 0 0 130px; color: var(--muted); font-size: 12.5px; }
  dl.kv dd { margin: 0; font-size: 13px; word-break: break-word; }

  #gate { max-width: 380px; margin: 80px auto; }
  #gate input { width: 100%; padding: 9px 11px; margin: 10px 0; border: 1px solid var(--line); border-radius: 7px; background: var(--panel); color: var(--ink); font-family: var(--mono); font-size: 13px; }
</style>
</head>
<body>

<div id="gate" class="card" hidden>
  <h2>Admin token</h2>
  <div style="padding:14px">
    <p class="msg">The portal prints this on startup.</p>
    <input id="token-input" type="password" placeholder="admin token" autocomplete="off">
    <button class="act primary" onclick="saveToken()">Connect</button>
    <span id="gate-msg" class="msg"></span>
  </div>
</div>

<div id="app" hidden>
  <header>
    <h1>PatchPanel</h1>
    <span class="rev" id="rev"></span>
    <span class="pill" id="paused" hidden onclick="resumeUpdates()"
      style="cursor:pointer"
      title="You are typing, or have something selected. The page refreshes every few seconds and that would discard it, so it is holding still. Click to resume now - anything unsaved in a field is kept, the selection is dropped.">live updates paused &mdash; resume</span>
    <nav>
      <button data-tab="overview" class="active">Overview<span class="badge" id="badge-overview" hidden></span></button>
      <button data-tab="machines">Machines<span class="badge" id="badge-machines" hidden></span></button>
      <button data-tab="schedule">Schedule</button>
      <button data-tab="jobs">Jobs<span class="badge" id="badge-jobs" hidden></span></button>
      <button data-tab="logs">Logs<span class="badge" id="badge-logs" hidden></span></button>
      <button data-tab="discovered">Network<span class="badge" id="badge-network" hidden></span></button>
      <button data-tab="backups">Backups<span class="badge" id="badge-backups" hidden></span></button>
      <button data-tab="setup">&#9881; Setup</button>
    </nav>
  </header>

  <main>
    <section id="overview" class="active">
      <div class="card">
        <h2>Needs you <span id="needs-sub" class="sub"></span></h2>
        <div id="needs"></div>
        <div class="proof" id="needs-none" hidden></div>
      </div>
      <div class="gist" id="gist"></div>
    </section>

    <section id="fleet">
      <div class="card">
        <h2>Machines</h2>
        <div class="step" style="padding-bottom:0">
          <input id="fleet-filter" placeholder="filter machines, pools, versions&hellip;"
            spellcheck="false" oninput="filterRows('fleet-filter','agents')"
            style="width:100%;padding:7px 10px;border:1px solid var(--line);border-radius:6px;background:var(--bg);color:var(--ink);font-size:12.5px">
        </div>
        <div class="scroll"><table>
          <thead><tr>
            <th>Machine</th><th>Pool</th><th>Updates</th>
            <th>Last patched</th><th>Seen</th><th></th>
          </tr></thead>
          <tbody id="agents"></tbody>
        </table></div>
        <div class="empty" id="agents-empty" hidden>No agents have enrolled yet.</div>
      </div>
      <div class="card">
        <h2>Look again <span class="sub">none of these change a machine</span></h2>
        <div class="bar">
          <button class="act" title="Re-read packages and updates on every connected machine. Changes nothing."
            onclick="broadcast('collect_inventory')">Rescan all</button>
          <button class="act" title="Re-probe every declared appliance now."
            onclick="broadcast('probe_devices')">Probe appliances</button>
          <button class="act" title="Sweep the manifest's discovery ranges for undeclared devices."
            onclick="broadcast('discover')">Run discovery</button>
          <span id="broadcast-msg" class="msg"></span>
        </div>
      </div>
    </section>

    <section id="agent">
      <div id="agent-body"></div>
    </section>

    <section id="backups">
      <div class="card">
        <h2>Backups</h2>
        <div class="step">
          <div class="note">
            Whether backups are happening, and whether anything is stuck. Read from each
            hypervisor by its own agent &mdash; what ran, from the job list, and what exists,
            from the backup storage. A job reporting success having written nothing is a real
            failure, so a guest's date comes from the files.
            <br><br>
            <b>A backup existing is not a backup restoring.</b> Nothing here is a test restore.
          </div>
          <div id="backup-tiles" class="tiles" style="margin-top:14px"></div>
        </div>
      </div>

      <div id="backup-alerts"></div>

      <div class="card">
        <h2>Guests</h2>
        <div class="scroll scrolly"><table>
          <thead><tr>
            <th>Guest</th><th>Host</th><th>Type</th><th>State</th>
            <th>Agent</th><th>Last backup</th><th></th>
          </tr></thead>
          <tbody id="backup-guests"></tbody>
        </table></div>
        <div class="empty" id="backup-empty" hidden>
          No hypervisors are reporting. A Proxmox host with an agent reports its guests and
          their backups automatically.
        </div>
      </div>

      <div id="backup-hosts"></div>
    </section>

    <section id="pools">
      <div class="card">
        <h2>Create a pool</h2>
        <div class="step">
          <div class="note">
            A pool decides what happens to a set of machines and when. A machine belongs to
            at most one, so there is always a single answer to &ldquo;why did that reboot&rdquo;.
            New pools patch <b>nothing</b> until you say otherwise.
          </div>
          <div class="bar" style="padding-left:0;padding-right:0;gap:10px;flex-wrap:wrap">
            <input id="pool-name" class="fld" style="width:180px" placeholder="name, e.g. servers">
            <select id="pool-scope" class="fld">
              <option value="none">patch nothing</option>
              <option value="security">security updates only</option>
              <option value="all">everything installable</option>
            </select>
            <select id="pool-reboot" class="fld">
              <option value="never">never reboot</option>
              <option value="if-needed">reboot if it asks for one</option>
            </select>
            <select id="pool-when" class="fld" onchange="poolWhenChanged()">
              <option value="manual">only when I press the button</option>
              <option value="daily">daily</option>
              <option value="weekly">weekly</option>
            </select>
            <select id="pool-dow" class="fld" hidden>
              <option value="0">Monday</option><option value="1">Tuesday</option>
              <option value="2">Wednesday</option><option value="3">Thursday</option>
              <option value="4">Friday</option><option value="5">Saturday</option>
              <option value="6">Sunday</option>
            </select>
            <input id="pool-time" class="fld" type="time" value="03:00" hidden>
            <label class="msg">at once <input id="pool-conc" class="fld" type="number"
              min="1" max="50" value="1" style="width:70px"></label>
            <button class="act primary" onclick="savePool()">Create</button>
            <span class="status" id="pool-status"></span>
          </div>
          <div class="msg">Times are the portal's local clock.</div>
        </div>
      </div>

      <div class="card">
        <h2>Schedule <span class="sub">three weeks back, two forward &mdash; the portal's local clock</span></h2>
        <div class="step">
          <div class="msg">Mostly the past, on purpose: what a schedule <em>will</em> do is
            derivable from the schedule itself, but what it <em>did</em> is not derivable from
            anything else. Only the next occurrence of each pool is spelled out; later ones
            are ticks.</div>
        </div>
        <div class="calwrap">
          <div class="calhead"><div>Mon</div><div>Tue</div><div>Wed</div><div>Thu</div><div>Fri</div><div>Sat</div><div>Sun</div></div>
          <div id="pool-calendar" class="cal"></div>
        </div>
        <div class="callegend">
          <span><em class="ink"></em> ran, installed something</span>
          <span><em></em> ran, nothing to install</span>
          <span><em class="bad"></em> failed</span>
          <span><em class="acc"></em> you did it by hand</span>
          <span><em style="border-top-style:dotted"></em> a job elsewhere checked in</span>
          <span><em class="next"></em> next occurrence</span>
        </div>
      </div>

      <div id="pool-list"></div>

      
      <div class="card">
        <h2>Membership</h2>
        <div class="scroll scrolly"><table>
          <thead><tr><th>Machine</th><th>OS</th><th>Pending</th><th>Pool</th></tr></thead>
          <tbody id="pool-members"></tbody>
        </table></div>
      </div>
    </section>

    <section id="jobs">
      <div id="job-status"></div>

      <div class="card">
        <h2>Jobs elsewhere <span class="sub">work PatchPanel does not do, reported by whatever does</span></h2>
        <div class="step">
          <div class="msg">A script that fails can tell you. A script that has
            <b>stopped running</b> cannot &mdash; and silence looks exactly like success.
            Tell PatchPanel how often each one should run and it will notice the silence
            on its behalf. Check-ins also appear on the <a href="#schedule"
            onclick="showTab('schedule');return false">schedule</a>, beside the runs
            PatchPanel does own.</div>
        </div>
        <div id="job-rows"></div>
        <div class="empty" id="jobs-empty" hidden>
          Nothing checks in yet. <a href="#" onclick="showJobSetup();return false">How to add one</a>.
        </div>
        <div class="step" id="job-setup" hidden></div>
      </div>

      <div class="card" id="job-history" hidden></div>
    </section>

    <section id="devices">
      <div class="card">
        <h2>Devices</h2>
        <div id="devices-eol" hidden></div>
        <div id="device-rows" class="devgrid"></div>
        <div class="empty" id="devices-empty" hidden>
          No devices yet. Add a firewall or NAS under <b>Add machine</b>.
        </div>
      </div>
      <div class="card" id="device-history" hidden></div>

    </section>

    <section id="logs">
      <div class="step">
        <div class="msg">Two sources, one place: appliances that can only push syslog,
          and machines whose agent has been asked to forward their journal. Turn a
          machine on from its own page.</div>
      </div>
      <div class="card">
        <h2>Device logs <span class="sub" id="log-sub"></span></h2>
        <div class="step">
          <div class="msg">Syslog from things that cannot host an agent. A rolling
            day, in one plain file per sender &mdash; this is not an archive, it is
            the context you read when something looks wrong.</div>
        </div>
        <div id="log-silent"></div>
        <div id="log-senders"></div>
        <div class="empty" id="log-empty" hidden></div>
      </div>

      <div class="card" id="live-card">
        <h2>Live
          <span class="sub" id="live-state"></span>
          <button class="act" id="live-full" style="float:right;padding:2px 8px;font-size:11.5px"
            onclick="fullLive()"
            title="Fill the screen with the feed. Escape comes back.">Full screen</button>
          <button class="act" id="live-toggle" style="float:right;padding:2px 8px;font-size:11.5px;margin-right:6px"
            onclick="toggleLive()">Start</button></h2>
        <div class="step" id="live-blurb">
          <div class="msg">Everything arriving from every sender at once, newest at the
            bottom. Scroll up to hold it still; scroll back to the bottom to follow
            again. This is a window on the last few thousand lines, not the archive
            &mdash; the per-sender files above are the record.</div>
        </div>
        <div class="live-controls">
          <input id="live-filter" placeholder="filter&hellip;" oninput="drawLive()"
            title="Show only lines containing this text. Applies to what has already arrived as well as what comes next.">
          <select id="live-who" onchange="drawLive()"
            title="One machine, or all of them."></select>
          <select id="live-sev" onchange="drawLive()" title="Severity, and worse.">
            <option value="7">everything</option>
            <option value="6">info and worse</option>
            <option value="4">warnings and worse</option>
            <option value="3">errors only</option>
          </select>
          <span class="msg" id="live-count"></span>
        </div>
        <div id="live-missed"></div>
        <div class="feed" id="live-feed" onscroll="liveScrolled()"
          onwheel="liveGesture()" ontouchmove="liveGesture()" onmousedown="liveGesture()"
          onkeydown="liveGesture()" tabindex="0"></div>
        <div class="empty" id="live-empty">Not running. Press Start.</div>
      </div>

      <div class="card" id="log-view" hidden></div>

    </section>

    <section id="discovered">
      <div class="step">
        <div class="msg">Everything that answered on the manifest's <code>discovery</code>
          ranges, whether PatchPanel manages it or not. Machines and declared appliances
          are named; anything nothing accounts for is marked, because that is the part
          worth a look. Click a row for what the scan learned about it.</div>
      </div>
      <div class="card">
        <h2>Seen on the network
          <button class="act" style="float:right;padding:2px 8px;font-size:11.5px"
            title="Sweep the manifest's discovery ranges again now, without waiting for the next automatic one. Read-only: it opens TCP connections, reads banners, and runs nmap on the ranges set to use it. It changes nothing."
            onclick="scanNow(this)">Scan now</button></h2>
        <div id="sweep-age"></div>
        <div id="network-note"></div>
        <div class="scroll"><table>
          <thead><tr><th>Address</th><th>What it is</th><th>Vendor</th><th>Open ports</th><th>Services</th><th>Found by</th></tr></thead>
          <tbody id="network-rows"></tbody>
        </table></div>
        <div class="empty" id="network-empty" hidden>
          Nothing has answered a sweep yet. Add a <code>discovery</code> range to the
          manifest, and the collector for its site will sweep it on its own schedule.
        </div>
      </div>


    </section>

    <section id="add">
      <div class="card">
        <h2>Add a machine</h2>
        <div class="step">
          <div class="note">
            Enrolling only makes a machine <b>report</b> what it has installed.
            Nothing is installed, upgraded, or rebooted until you publish a
            manifest that asks for it.
          </div>
          <div class="field">
            <label for="add-portal">Portal address</label>
            <input id="add-portal" spellcheck="false" size="24">
            <label for="add-site" style="margin-left:8px">Site</label>
            <input id="add-site" value="homelab" spellcheck="false" size="14">
          </div>
          <div id="add-hint" class="msg"></div>
        </div>
      </div>

      <div class="card">
        <h2>Linux &mdash; run as root</h2>
        <div class="step">
          <div class="cmd"><button onclick="copyCmd('cmd-linux')">Copy</button><pre id="cmd-linux"></pre></div>
        </div>
      </div>

      <div class="card">
        <h2>Windows &mdash; run in an elevated PowerShell</h2>
        <div class="step">
          <div class="cmd"><button onclick="copyCmd('cmd-win')">Copy</button><pre id="cmd-win"></pre></div>
          <p style="margin-top:10px">For Windows Update patching as well as winget app updates, the
             target also needs <code>Install-Module PSWindowsUpdate -Force -Scope AllUsers</code>.</p>
        </div>
      </div>

      <div class="card">
        <h2>What the installer does</h2>
        <div class="step">
          <p>Downloads the agent from this portal, enrols it, installs a service that
             restarts on failure, and starts it. Re-running upgrades in place and keeps
             the machine's existing identity.</p>
        </div>
      </div>
      <div class="card">
        <h2>Appliances &mdash; no agent</h2>
        <div class="step">
          <div class="note">
            A firewall or a NAS is the machine you least want to install software on, and
            often cannot: they run from read-only images, or wipe anything added at the next
            firmware update. PatchPanel asks these over their own API instead. Nothing is
            installed and nothing is written to the device.
          </div>
          <div class="bar" style="padding-left:0;padding-right:0;gap:10px;flex-wrap:wrap">
            <select id="dev-kind" onchange="deviceKindChanged()" class="fld">
              <option value="opnsense">OPNsense firewall</option>
              <option value="unraid">Unraid server</option>
              <option value="homeassistant">Home Assistant</option>
            </select>
            <input id="dev-url" class="fld" style="min-width:320px"
              placeholder="https://router.example.net" oninput="deviceUrlChanged()">
            <input id="dev-name" class="fld" style="width:190px" placeholder="name, e.g. edge firewall">
            <input id="dev-id" class="fld" style="min-width:260px" placeholder="id"
              oninput="this.dataset.touched = '1'">
          </div>

          <div class="bar" style="padding-left:0;padding-right:0;gap:10px;flex-wrap:wrap">
            <label class="msg" for="dev-keyfile" id="dev-keyfile-label">API key file</label>
            <input type="file" id="dev-keyfile" accept=".txt,text/plain"
              onchange="readKeyFile(this)" class="fld">
            <span class="status" id="dev-key-status"></span>
          </div>
          <div class="bar" style="padding-left:0;padding-right:0;gap:10px;flex-wrap:wrap">
            <label class="msg" for="dev-key">or paste the key</label>
            <input id="dev-key" class="fld" style="min-width:340px" placeholder="api key"
              oninput="pastedKey()">
            <input id="dev-secret" class="fld" style="min-width:340px" placeholder="api secret"
              oninput="pastedKey()">
          </div>

          <div class="bar" style="padding-left:0;padding-right:0">
            <button class="act primary" onclick="addDevice()">Add device</button>
            <span class="status" id="dev-add-status"></span>
          </div>
          <div class="msg" id="dev-hint"></div>
        </div>
      </div>


    </section>

    <section id="manifest">
      <div class="card">
        <h2>Desired state</h2>
        <textarea id="manifest-doc" spellcheck="false"></textarea>
        <div class="bar">
          <button class="act primary" onclick="saveManifest()">Publish</button>
          <button class="act" onclick="loadManifest()">Reload</button>
          <span id="manifest-msg" class="msg"></span>
        </div>
        <div class="step">
          <div class="msg">Publishing raises the revision; each machine picks it up on its
            next check-in. To push it now, across the whole fleet:</div>
          <div class="bar">
            <button class="act" title="Install, upgrade or remove applications on every connected machine so it matches the manifest above. This changes systems."
              onclick="broadcast('apply_manifest')">Apply manifest to all</button>
            <span id="broadcast-msg2" class="msg"></span>
          </div>
        </div>
      </div>
      <div class="card">
        <h2>Agent builds</h2>
        <div class="scroll"><table>
          <thead><tr><th>Version</th><th>OS</th><th>Arch</th><th>URL</th><th>sha256</th></tr></thead>
          <tbody id="build-rows"></tbody>
        </table></div>
        <div class="empty" id="builds-empty" hidden>
          No builds published. POST to <code>/api/builds</code>, then set
          <code>agent_version</code> in the manifest to roll the fleet.
        </div>
      </div>
    </section>

    <section id="activity">
      <div class="card">
        <h2>Recent commands</h2>
        <div id="commands"></div>
        <div class="empty" id="commands-empty" hidden>Nothing has run yet.</div>
      </div>
    </section>
  </main>
</div>

<script>
const $ = (id) => document.getElementById(id);
let TOKEN = localStorage.getItem("pp_token") || "";
// Read from the URL so a refresh, a bookmark, or a shared link all land on the
// tab you were actually looking at.
// A tab is a question, and more than one card can answer it: Machines shows
// the agents and the appliances, because "what is everything running" does not
// care which of them has an agent installed.
const TAB_SECTIONS = {
  overview: ["overview"],
  machines: ["fleet", "devices"],
  schedule: ["pools"],
  jobs: ["jobs"],
  logs: ["logs"],
  discovered: ["discovered"],
  backups: ["backups"],
  setup: ["add", "manifest", "activity"],
};
const TABS = Object.keys(TAB_SECTIONS);
// Links, bookmarks and muscle memory from the seven-tab layout still land
// somewhere sensible rather than silently falling back to the first tab.
const MOVED = { fleet: "machines", devices: "machines", pools: "schedule",
  network: "discovered", unmanaged: "discovered",
  add: "setup", manifest: "setup", activity: "setup" };
// `#agent/<uuid>` opens one machine's page; anything else is a tab.
// `#agent/<uuid>` opens a machine, `#agent/<uuid>/<pane>` opens it on one of
// its tabs, so a bookmark or a shared link lands exactly where you were.
function routeOf(hash) {
  const h = (hash || "").replace(/^#/, "");
  if (h.startsWith("agent/")) {
    const [id, pane] = h.slice(6).split("/");
    return { tab: "agent", id, pane: pane || null };
  }
  const tab = TABS.includes(h) ? h : (MOVED[h] || "overview");
  return { tab, id: null, pane: null };
}
let ROUTE = routeOf(location.hash);
let TAB = ROUTE.tab;
let PANE = ROUTE.pane || "overview";
let AGENTS = [];
let REV = 0;
let AUTH_REQUIRED = true;
// Agents we have just sent a command to. The portal only learns a command is
// running once it records it, and the table refreshes every few seconds, so
// without a local latch the same button can be pressed twice in that window.
const JUST_SENT = new Map();
const SENT_GRACE_MS = 20000;

function isBusy(a) {
  if (a.running) return a.running.kind;
  const t = JUST_SENT.get(a.id);
  if (t && Date.now() - t < SENT_GRACE_MS) return "dispatching";
  if (t) JUST_SENT.delete(a.id);
  return null;
}

// Human wording for a command kind.
const KIND_LABEL = {
  collect_inventory: "scanning",
  apply_patches: "installing updates",
  apply_manifest: "applying manifest",
  self_update: "updating agent",
  probe_devices: "probing devices",
  discover: "discovering",
  cleanup: "cleaning up",
  distro_check: "checking upgrade readiness",
  finish_upgrade: "finishing the upgrade",
  update_firmware: "flashing firmware",
  distro_upgrade: "upgrading the release",
  reboot: "rebooting",
  dispatching: "dispatching",
};

async function api(path, opts = {}) {
  const res = await fetch(path, {
    ...opts,
    headers: { "Authorization": "Bearer " + TOKEN, "Content-Type": "application/json", ...(opts.headers || {}) },
  });
  if (res.status === 401 && AUTH_REQUIRED) {
    gate("That token was not accepted.");
    throw new Error("unauthorized");
  }
  const body = await res.json().catch(() => ({}));
  if (!res.ok) throw new Error(body.error || res.statusText);
  return body;
}

function gate(msg) {
  $("app").hidden = true;
  $("gate").hidden = false;
  $("gate-msg").textContent = msg || "";
  $("gate-msg").className = "msg bad";
}

function saveToken() {
  TOKEN = $("token-input").value.trim();
  localStorage.setItem("pp_token", TOKEN);
  $("gate").hidden = true;
  $("app").hidden = false;
  refresh();
}

document.addEventListener("input", (e) => {
  if (e.target && (e.target.id === "add-site" || e.target.id === "add-portal")) loadAdd();
});

function showTab(tab, id, pane) {
  ROUTE = { tab, id: id || null, pane: pane || null };
  TAB = tab;
  if (tab === "agent") PANE = pane || "overview";
  const want = id ? `agent/${id}${pane ? "/" + pane : ""}` : tab;
  // No nav button is highlighted on a detail page; it is not a tab.
  document.querySelectorAll("nav button").forEach((x) =>
    x.classList.toggle("active", x.dataset.tab === tab));
  const shown = TAB_SECTIONS[tab] || [tab];
  document.querySelectorAll("section").forEach((s) =>
    s.classList.toggle("active", shown.includes(s.id)));
  if (location.hash.slice(1) !== want) location.hash = want;
  // Opening the Logs tab is the whole intent behind a live view, so it starts
  // itself rather than asking. It is stopped by hand and never restarted
  // automatically after that - having pressed Stop, being overruled by a tab
  // change would be worse than the extra click.
  if (tab === "logs" && !LIVE_ON && !LIVE_STOPPED) toggleLive();
  refresh();
}

function openAgent(id) { showTab("agent", id); }

// The machine currently being compared against on the Sources tab, and its
// data. Held outside the render so the five-second refresh does not drop it.
// A scan is not a change, so the history hides them by default - but they are
// the record of when a machine was last actually looked at, which is worth
// being able to see.
let HISTORY_ALL = false;

function setHistoryAll(on) {
  HISTORY_ALL = !!on;
  refresh();
}

let COMPARE = null;
let COMPARE_DATA = null;

async function setCompare(id) {
  COMPARE = id || null;
  COMPARE_DATA = null;
  if (COMPARE) {
    try {
      COMPARE_DATA = await api(`/api/agents/${COMPARE}`);
    } catch (e) {
      COMPARE = null;
      alert(e.message);
    }
  }
  refresh();
}

// Switching pane is a pure DOM toggle: every pane is already rendered, so the
// change is instant and nothing you had open, filtered, or typed is lost.
function showPane(pane) {
  PANE = pane;
  ROUTE.pane = pane;
  const want = `agent/${ROUTE.id}/${pane}`;
  if (location.hash.slice(1) !== want) location.hash = want;
  applyPane();
}

function applyPane() {
  const body = $("agent-body");
  const panes = [...body.querySelectorAll("[data-pane]")];
  if (!panes.length) return;
  // A machine with no apt sources has no Sources pane; fall back rather than
  // showing a blank page to anyone who kept the link.
  if (!panes.some((e) => e.dataset.pane === PANE)) PANE = panes[0].dataset.pane;
  panes.forEach((e) => { e.hidden = e.dataset.pane !== PANE; });
  body.querySelectorAll(".subnav button").forEach((b) =>
    b.classList.toggle("active", b.dataset.p === PANE));
}

document.querySelectorAll("nav button").forEach((b) => {
  b.onclick = () => showTab(b.dataset.tab);
});

window.addEventListener("hashchange", () => {
  const r = routeOf(location.hash);
  if (r.tab !== ROUTE.tab || r.id !== ROUTE.id) { showTab(r.tab, r.id, r.pane); return; }
  // Same machine, different pane (a back button press, usually): no reload.
  if (r.tab === "agent" && (r.pane || "overview") !== PANE) {
    PANE = r.pane || "overview";
    ROUTE.pane = r.pane;
    applyPane();
  }
});

// Re-rendering on a timer is what makes the dashboard live, and also what
// closes an <details> you were reading. Two defences: skip the write entirely
// when nothing changed, and carry the open ones across when it did.
// True while the operator has unsaved text in any editor on the page.
// Re-rendering under them would discard it, and this page refreshes every few
// seconds, which made editing a sources file effectively impossible.
function isEditing() {
  return [...document.querySelectorAll("textarea[data-dirty=\"1\"]")].length > 0;
}

/// Is the operator in the middle of something inside this element?
///
/// Restoring focus and the caret after a rewrite is not enough - the field is
/// a different element afterwards, the page reflows, and anything highlighted
/// is gone. Typing, a dropdown they are working with, or text they have
/// selected to copy all mean the same thing: leave the page alone until they
/// are done with it.
function typingIn(el) {
  if (!el) return false;

  const a = document.activeElement;
  if (a && el.contains(a)) {
    if (a.tagName === "TEXTAREA" || a.tagName === "INPUT" || a.tagName === "SELECT") return true;
    if (a.isContentEditable) return true;
  }

  // Selected text lives only in the DOM nodes it spans; replacing them drops
  // it, which is maddening halfway through copying an error message.
  try {
    const sel = window.getSelection && window.getSelection();
    if (sel && sel.rangeCount && !sel.isCollapsed) {
      const r = sel.getRangeAt(0);
      if (el.contains(r.commonAncestorContainer)) return true;
    }
  } catch (e) {
    // No selection API here; nothing to preserve.
  }
  return false;
}

function setHTML(el, html) {
  if (!el) return false;
  if (typingIn(el)) {
    // Say so, rather than leaving a page that has quietly stopped updating.
    // Only ever raised here; `refresh` lowers it at the start of each pass, so
    // the state is recomputed rather than remembered.
    showPaused(true);
    return false;
  }
  if (el.__lastHTML === html) return false;
  const open = new Set(
    [...el.querySelectorAll("details[open][data-k]")].map((d) => d.dataset.k)
  );
  const active = document.activeElement;
  const keepId = active && el.contains(active) ? active.id : null;
  const caret = keepId && "selectionStart" in active ? active.selectionStart : null;

  // A running command's output grows every few seconds, and rewriting the
  // element sends its scrollbar back to the top - so a long job like a release
  // upgrade shows you its first minute, over and over, while the part you want
  // is the end. Remember where each log was and put it back; a log that was at
  // the bottom stays at the bottom, which is what following output means.
  const logs = new Map();
  el.querySelectorAll("pre[data-k]").forEach((pre) => {
    logs.set(pre.dataset.k, {
      bottom: pre.scrollHeight - pre.scrollTop - pre.clientHeight < 24,
      top: pre.scrollTop,
    });
  });

  // Replacing the contents collapses the page to nothing for an instant, and
  // the browser clamps the scroll position to the now much shorter document -
  // so reading anything below the fold meant being thrown back to the top
  // every few seconds.
  //
  // Reading scrollY back straight afterwards is not enough: layout has not
  // been recalculated yet, so it still reports the old value and the clamp
  // lands after the check. Hold the element's height across the swap so the
  // document never shrinks, and restore unconditionally either side of the
  // next frame.
  const y = window.scrollY;
  const held = el.offsetHeight;
  if (held) el.style.minHeight = held + "px";

  el.__lastHTML = html;
  el.innerHTML = html;

  window.scrollTo(0, y);
  requestAnimationFrame(() => {
    el.style.minHeight = "";
    if (Math.abs(window.scrollY - y) > 1) window.scrollTo(0, y);
  });

  el.querySelectorAll("pre[data-k]").forEach((pre) => {
    const was = logs.get(pre.dataset.k);
    // Anything still running is followed from the moment it appears.
    if (!was) {
      if (pre.dataset.live === "1") pre.scrollTop = pre.scrollHeight;
      return;
    }
    pre.scrollTop = was.bottom ? pre.scrollHeight : was.top;
  });

  el.querySelectorAll("details[data-k]").forEach((d) => {
    if (open.has(d.dataset.k)) d.open = true;
  });
  if (keepId) {
    const again = document.getElementById(keepId);
    if (again) {
      again.focus();
      if (caret !== null && "setSelectionRange" in again) {
        try { again.setSelectionRange(caret, caret); } catch (e) { /* not a text field */ }
      }
    }
  }
  return true;
}

function tailLog(details) {
  const pre = details.querySelector("pre[data-live=\"1\"]");
  if (pre && details.open) pre.scrollTop = pre.scrollHeight;
}

// Let go of whatever is holding the page still.
//
// Unsaved text in a field is left alone - it is only the focus and the
// selection that block a redraw, and an edited source file is guarded
// separately by its own dirty flag.
function resumeUpdates() {
  try {
    const sel = window.getSelection && window.getSelection();
    if (sel) sel.removeAllRanges();
  } catch (e) {
    // No selection API; nothing to release.
  }
  const active = document.activeElement;
  if (active && active.blur) active.blur();
  showPaused(false);
  refresh();
}

function showPaused(on) {
  const el = $("paused");
  if (el) el.hidden = !on;
}

// A string safe to drop into an inline handler.
//
// `JSON.stringify` produces double quotes, and a double quote inside a
// double-quoted HTML attribute ends the attribute - the browser then throws
// SyntaxError on click and the button silently does nothing. This quotes with
// apostrophes and escapes for both layers, JavaScript first and HTML second.
function jsq(value) {
  const js = String(value ?? "")
    .replace(/\\/g, "\\\\")
    .replace(/'/g, "\\'")
    .replace(/\r?\n/g, "\\n");
  return "'" + js.replace(/&/g, "&amp;").replace(/"/g, "&quot;").replace(/</g, "&lt;") + "'";
}

// Sizes that stay honest at both ends. Rounding to KB turns a real file with
// forty bytes in it into "0 KB", which reads as "nothing has arrived".
function size(bytes) {
  const n = Number(bytes) || 0;
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1048576).toFixed(1)} MB`;
}

const esc = (s) => String(s ?? "").replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));

function ago(iso) {
  if (!iso) return "-";
  const s = Math.max(0, (Date.now() - new Date(iso)) / 1000);
  if (s < 60) return Math.round(s) + "s ago";
  if (s < 3600) return Math.round(s / 60) + "m ago";
  if (s < 86400) return Math.round(s / 3600) + "h ago";
  return Math.round(s / 86400) + "d ago";
}

function tile(n, label, cls) {
  return `<div class="tile ${n > 0 && cls ? cls : ""}"><div class="n">${n}</div><div class="l">${label}</div></div>`;
}

async function loadFleet() {
  const d = await api("/api/fleet");
  AGENTS = d.agents;
  REV = d.manifest_revision;
  $("rev").textContent = "manifest r" + d.manifest_revision;

  const s = d.summary;

  // A count of machines and devices with something waiting. Deliberately a
  // count of *things needing attention*, not of updates: forty packages on one
  // box is one thing to go and do.
  const badge = (id, n, bad, title) => {
    const el = $(id);
    if (!el) return;
    el.hidden = false;
    el.textContent = n > 99 ? "99+" : String(n);
    el.className = "badge" + (n === 0 ? " ok" : bad ? " bad" : "");
    el.title = title;
  };
  // The badge counts what needs a person, not what is pending: a machine a
  // pool patches tonight is not something to nag about.
  const worst = (d.attention || []).reduce((w, i) => Math.min(w, i.tier), 9);
  badge("badge-overview", (d.attention || []).length, worst <= 2,
    (d.attention || []).length
      ? `${d.attention.length} thing(s) need you`
      : "nothing needs you");
  badge("badge-jobs", s.jobs_bad || 0, (s.jobs_bad || 0) > 0,
    (s.jobs_bad || 0)
      ? `${s.jobs_bad} job(s) failing or not checking in`
      : "every job is reporting on time");
  // Amber, not red: an unexplained host is worth a look, not an alarm.
  badge("badge-network", s.unexplained_hosts || 0, false,
    (s.unexplained_hosts || 0)
      ? `${s.unexplained_hosts} host(s) answering that nothing here accounts for`
      : "everything answering is accounted for");
  // Red, because this one means a machine was asked to send logs and has not -
  // which is a fault in PatchPanel, not in the machine.
  // Amber, not red. "Asked and silent" usually means a quiet machine, and
  // only sometimes means a fault - red is reserved for "do not trust what this
  // page says", which this is not.
  badge("badge-logs", s.logs_silent || 0, false,
    (s.logs_silent || 0)
      ? `${s.logs_silent} machine(s) asked to forward have sent nothing for over half an hour`
      : "every machine asked to forward is sending");
  badge("badge-backups", s.backups_at_risk || 0, (s.backups_at_risk || 0) > 0,
    (s.backups_at_risk || 0)
      ? `${s.backups_at_risk} running guest(s) with no recent backup`
      : "nothing running is unprotected");
  // Machines is a reference view; a reference view does not nag. The only
  // thing it carries is an appliance nobody else will speak for.
  badge("badge-machines", s.devices_eol || 0, (s.devices_eol || 0) > 0,
    `${s.devices_eol || 0} appliance(s) past end of life`);
  drawNeeds(d);

  $("agents-empty").hidden = d.agents.length > 0;
  const agentRows = d.agents.map((a) => {
    const live = (a.connected || a.online) ? "on"
      : (Date.now() - new Date(a.last_seen).getTime() > 864e5 ? "bad" : "off");
    const busy = isBusy(a);
    const canAct = a.connected && !busy;
    const busyPill = busy
      ? ` <span class="pill busy" title="A command is already running on this machine. Wait for it to finish before starting another.">${esc(KIND_LABEL[busy] || busy)}</span>`
      : "";
    // Show the total, then flag the security subset in full words. The old
    // form rendered "1 sec 0" - which reads as a duration, and buried the
    // total behind an unexplained subtraction.
    const unsafe_ = "";
    const actionable = a.actionable_count;
    const total = a.update_count;
    // Phased and held-back explain the number; they are not separate things to
    // act on. They belong in its tooltip rather than beside it, where they were
    // two more pills on a row that already had five.
    const why = [
      a.deferred_count
        ? `${a.deferred_count} phased - the archive is withholding them from this machine until the rollout reaches it. Nothing installs them; they arrive on their own.`
        : "",
      a.held_back_count ? `${a.held_back_count} held back - needs a full upgrade.` : "",
      a.ignored_count ? `${a.ignored_count} ignored by you.` : "",
    ].filter(Boolean).join(" ");
    const sec = a.security_count
      ? ` <span class="sec" title="${a.security_count} of these are security updates - patch these first">&middot; ${a.security_count} security</span>`
      : "";
    // One format, and it never changes shape between machines: the number you
    // can act on, and - when the archive is withholding the difference - what
    // it is a fraction of. "nothing to install" was one word away from "none",
    // which hid exactly the distinction that matters.
    const ignored = a.ignored_count
      ? ` <span class="of">&middot; ${a.ignored_count} ignored</span>`
      : "";
    // Zero updates from a machine we could not scan is not "clean", it is
    // "unknown". Showing 0 there is the most dangerous thing this table could do.
    const upd = a.scan_issue_count
      ? `<span class="pill bad" title="Some package backends could not be scanned on this machine, so the real number is unknown. Open the machine for details.">not scanned</span>`
      : total === 0
        ? `<span class="msg">none</span>${ignored}`
        : `<span title="${esc(why)}">${actionable}</span>${
            actionable === total ? "" : ` <span class="of" title="${esc(why)}">of ${total}</span>`
          }${sec}${ignored}`;
    const dev = a.device_count
      ? `${a.device_count}${a.device_problem_count ? ` <span class="pill bad">${a.device_problem_count}</span>` : ""}`
      : "-";
    const hw = a.hardware || {};
    // Hardware lives on the machine's own page; in a fleet list it is a very
    // wide column nobody scans. Keep it reachable as a tooltip.
    const spec = [
      hw.cpu_model,
      hw.cpu_threads ? `${hw.cpu_cores || "?"}c/${hw.cpu_threads}t` : "",
      hw.memory_mb ? `${(hw.memory_mb / 1024).toFixed(0)} GB` : "",
      hw.vendor,
    ].filter(Boolean).join(" · ");
    // The address is worth having, but not worth a column: nobody scans a
    // list of IPs, they look one up.
    const ipText = (hw.ip_addresses || []).join(", ");
    // One pill per row, and it is the row's verdict rather than a label for a
    // field: the highest-ranked thing true about this machine. Everything it
    // displaced is either context on the quiet second line or a tooltip on the
    // number it explains.
    //
    // Red does not mean severe here. Red means "do not trust the green on this
    // row" - something failed, or the number is not known to be true. Amber
    // means true, and waiting on a person. Held-back upgrades are deliberately
    // absent: seven of thirteen machines have them, so a pill for it would put
    // amber on half the table and say nothing. It lives in the number's
    // tooltip, and gets one row of its own on Overview.
    const VERDICTS = [
      [a.mid_upgrade, "bad", "mid-upgrade",
        "dpkg is part-way through an upgrade. Nothing else will run on this machine until it is finished or rolled back."],
      [a.release_blockers > 0, "bad", "unsafe sources",
        "This machine's package sources are misconfigured; installing updates could break it."],
      [a.patch_state === "failed", "bad", "last run failed", a.patch_note],
      [a.blocked_count > 0, "bad", `${a.blocked_count} blocked`,
        `${a.blocked_count} update(s) a patch run was asked to install and that did not move.`],
      [a.patch_state === "missed", "warn", "missed its run", a.patch_note],
      [a.reboot_required, "warn", "reboot",
        "This machine is waiting for a restart to finish applying updates."],
      [!a.pool && actionable > 0, "warn", "no pool",
        "Nothing patches this machine on a schedule."],
    ];
    const v = VERDICTS.find((x) => x[0]);
    const extras = v
      ? ` <span class="pill ${v[1]}" title="${esc(v[3] || "")}">${esc(v[2])}</span>`
      : "";
    const context = [
      a.guest_count ? `${a.guest_count} VMs` : "",
      a.device_count ? `${a.device_count} devices${a.device_problem_count ? " (!)" : ""}` : "",
      a.drift_count ? `${a.drift_count} drift` : "",
      a.applied_revision < REV ? `manifest r${a.applied_revision}` : "",
    ].filter(Boolean).join(" · ");

    return `<tr data-n="${esc(a.hostname.toLowerCase())} ${esc((a.pool || "").toLowerCase())} ${esc(a.os_version.toLowerCase())} ${esc(a.patch_state || "")}">
      <td class="machine"><span class="dot ${live}"></span><a href="#agent/${a.id}" style="color:inherit"
        title="${esc(spec)}${ipText ? " &middot; " + esc(ipText) : ""}">${esc(a.hostname)}</a>${extras}
        <div class="msg">${esc(a.os_version)} <span class="mono">${esc(a.arch)}</span>${
          context ? ` &middot; ${esc(context)}` : ""}</div></td>
      <td>${a.pool
        ? `<a href="#pools" style="color:inherit">${esc(a.pool)}</a>`
        : '<span class="msg" title="No pool: nothing patches this machine on a schedule.">none</span>'}</td>
      <td class="num">${upd}${unsafe_}${busyPill}
        ${a.patch_short && a.patch_state !== "failed" && a.patch_state !== "missed"
          ? `<div class="msg" title="${esc(a.patch_note)}">${esc(a.patch_short)}</div>`
          : ""}</td>
      <td>${a.last_patched
        ? `${ago(a.last_patched)}${a.last_patch_ok === false ? ' <span class="pill bad">failed</span>' : ""}`
        : '<span class="msg">never</span>'}</td>

      <td>${ago(a.last_seen)}</td>
      <td style="text-align:right; white-space:nowrap">
        <button class="act" ${canAct ? "" : "disabled"}
          title="${busy ? "Busy: " + esc(KIND_LABEL[busy] || busy) : "Re-read installed packages and check for available updates. Changes nothing."}"
          onclick="cmd('${a.id}','collect_inventory')">Rescan</button>
        <button class="act" ${canAct && actionable ? "" : "disabled"}
          title="${busy
            ? "Busy: " + esc(KIND_LABEL[busy] || busy)
            : (actionable
                ? "Install this machine's pending OS updates now. This changes the system and may require a reboot."
                : (a.deferred_count
                    ? "Nothing to install: the " + a.deferred_count + " pending update(s) are phased and the archive is withholding them from this machine."
                    : "Nothing to install."))}"
          onclick="patchNow('${a.id}','${esc(a.hostname)}')">Install updates</button>
        <button class="act" ${canAct ? "" : "disabled"}
          title="${busy ? "Busy: " + esc(KIND_LABEL[busy] || busy) : "Install, upgrade or remove applications so the machine matches the manifest."}"
          onclick="cmd('${a.id}','apply_manifest')">Apply manifest</button>
      </td>
    </tr>`;
  }).join("");
  setHTML($("agents"), agentRows);
}

function since(iso) {
  if (!iso) return "-";
  return dur(Math.max(0, (Date.now() - new Date(iso)) / 1000));
}

// A span of seconds, coarsest two units. Kept separate from `since` because an
// uptime arrives as a duration already, and rounding it to whole days would
// hide the case that matters most - something that rebooted an hour ago.
function dur(s) {
  const d = Math.floor(s / 86400), h = Math.floor((s % 86400) / 3600), m = Math.floor((s % 3600) / 60);
  if (d) return `${d}d ${h}h`;
  if (h) return `${h}h ${m}m`;
  return `${m}m`;
}

// Join the non-empty parts of a list for display, dropping repeats.
//
// The separator is markup and each part is content, so the parts are escaped and
// the separator is not - doing it the other way round renders a literal
// "&middot;" on the page, which is how this got noticed. Duplicates go because
// nmap classes a machine as "general purpose / Linux / Linux" and three fields
// agreeing is not three facts.
function dotted(parts) {
  const seen = [];
  for (const p of parts) {
    if (p && !seen.includes(p)) seen.push(p);
  }
  return seen.map(esc).join(" &middot; ");
}

function kv(rows) {
  return `<dl class="kv">` + rows
    .filter(([, v]) => v !== null && v !== undefined && v !== "")
    .map(([k, v]) => `<div><dt>${k}</dt><dd>${v}</dd></div>`)
    .join("") + `</dl>`;
}

// Updates somebody decided not to install.
//
// Pinned to the version they decided about, so this is a judgement about one
// release rather than a package silently disappearing from the fleet's view
// forever. When a newer version is published it comes back by itself.
function ignoredCard(ignored, id) {
  if (!ignored.length) return "";
  return `<div class="card">
    <h2>Ignored &mdash; ${ignored.length}</h2>
    <div class="step">
      <div class="msg">These are not counted as pending. Each is set aside at the exact version
        below; if a newer one is published it appears again as a new update.</div>
    </div>
    <div class="scroll scrolly"><table>
      <thead><tr><th>Package</th><th>Version ignored</th><th>Source</th><th>Since</th><th></th></tr></thead>
      <tbody>${ignored.map((x) => `<tr>
        <td>${esc(x.name)}</td>
        <td class="mono">${esc(x.version)}</td>
        <td class="mono msg">${esc(x.source)}</td>
        <td class="msg">${ago(x.ignored_at)}</td>
        <td><button class="act" style="padding:2px 8px;font-size:11.5px"
          onclick="unignoreUpdate(${jsq(id)}, ${jsq(x.name)}, ${jsq(x.version)})">Show again</button></td>
      </tr>`).join("")}</tbody>
    </table></div>
  </div>`;
}

async function ignoreUpdate(id, name, source, version) {
  if (!confirm(
    "Stop showing " + name + " " + version + " on this machine?\n\n" +
    "It will not be counted as pending, and Install updates will still install it if you " +
    "run one. If a newer version is published it comes back."
  )) return;
  try {
    await api(`/api/agents/${id}/ignores`, {
      method: "POST",
      body: JSON.stringify({ name, source, version }),
    });
    refresh();
  } catch (e) {
    alert(e.message);
  }
}

async function unignoreUpdate(id, name, version) {
  try {
    await api(`/api/agents/${id}/ignores`, {
      method: "DELETE",
      body: JSON.stringify({ name, version }),
    });
    refresh();
  } catch (e) {
    alert(e.message);
  }
}

function scanCard(issues, held, deferred, id, connected) {
  if (!issues.length && !held.length && !deferred.length) return "";
  return `<div class="card">
    <h2>Coverage</h2>
    ${issues.length ? `<div class="step">
      <div class="note" style="border-color:var(--bad)">
        <b>This machine's update count is incomplete.</b> ${issues.length} backend(s)
        could not be scanned, so treat the number as a floor, not a total.
      </div>
      ${issues.map((i) => `<div style="margin-top:10px">
        <h3><span class="pill bad">${esc(i.backend)}</span> ${esc(i.problem)}</h3>
        ${i.remedy ? `<pre>${esc(i.remedy)}</pre>` : ""}
      </div>`).join("")}
      ${issues.some((i) => i.backend === "winget" || i.backend === "windowsupdate" || i.backend === "fwupd")
        ? `<button class="act" ${connected ? "" : "disabled"} style="margin-top:10px"
             title="Installs whatever this machine needs to be fully scannable: fwupd on Linux, PSWindowsUpdate and a system-wide winget on Windows."
             onclick="installPrereqs('${id}')">Install the missing tooling</button>
           <button class="act" ${connected ? "" : "disabled"} style="margin-top:10px"
             title="Backends are detected once at startup, so a restart is needed after installing tooling."
             onclick="restartAgent('${id}')">Restart agent</button>`
        : ""}
    </div>` : ""}
    ${deferred.length ? `<div class="step">
      <h3><span class="pill">phased</span> ${deferred.length} update(s) the archive is withholding</h3>
      <p>Ubuntu rolls an update out to a percentage of machines at a time, and this machine is not
         in the cohort yet. Nothing installs these &mdash; not <code>apt upgrade</code>, not
         <code>full-upgrade</code> &mdash; until the rollout reaches it, which it will on its own.
         Some of them apt merely calls "kept back"; they are listed here because what they are
         waiting on is itself phased, so a full upgrade cannot help them either. There is nothing
         to fix, and a patch run that appears to do nothing is in fact correct.</p>
      <pre>${esc(deferred.join(" "))}</pre>
    </div>` : ""}
    ${held.length ? `<div class="step">
      <h3><span class="pill warn">held back</span> ${held.length} upgrade(s) apt will not apply</h3>
      <p>A plain <code>apt upgrade</code> refuses anything needing new packages installed &mdash;
         typically a kernel metapackage. These stay pending forever until a full upgrade runs,
         which may also remove packages, so it is a deliberate action.</p>
      <ul style="margin:8px 0 0 18px">${held.map((h) => `<li class="mono">${esc(h)}</li>`).join("")}</ul>
      <button class="act" ${connected ? "" : "disabled"}
        title="Runs apt full-upgrade. This can install new packages and remove existing ones."
        onclick="fullUpgrade('${id}')">Run full upgrade</button>
    </div>` : ""}
  </div>`;
}

function cleanupCard(c, id, connected) {
  if (!c || (!(c.packages || []).length && !c.cache_bytes)) return "";
  const mb = (n) => `${(n / 1e6).toFixed(0)} MB`;
  return `<div class="card">
    <h2>Cleanup</h2>
    <div class="step">
      <p>${(c.packages || []).length} package(s) are installed only as dependencies nothing
         needs any more${c.reclaim_bytes ? `, holding ${mb(c.reclaim_bytes)}` : ""}.
         The package cache holds a further ${mb(c.cache_bytes || 0)}, which is always safe to delete.</p>
      ${(c.packages || []).length ? `<pre>${esc(c.packages.join(" "))}</pre>` : ""}
      <button class="act" ${connected ? "" : "disabled"}
        title="Runs apt autoremove and empties the package cache."
        onclick="cleanupNow('${id}')">Clean up</button>
    </div>
  </div>`;
}

// A machine dpkg stopped part-way through an upgrade.
//
// This is the state that looks like nothing is wrong: the update list is
// normal, the machine is up, and every single install fails. It goes at the
// top of the page because until it is resolved nothing else on the machine can
// be done at all.
function midUpgradeCard(mid, id, connected, busy) {
  if (!mid || !(mid.packages || []).length) return "";

  const disks = mid.disks || [];
  const picker = mid.grub_stuck && disks.length
    ? `<div class="step">
        <h3>Which disk should the bootloader be installed to?</h3>
        <p>The recorded answer points at a device that no longer exists, which is why
           <span class="mono">grub</span> could not finish. This is usually the disk the
           machine boots from. Getting it wrong does not damage anything, but the machine
           may not boot until it is corrected, so check it against the sizes below.</p>
        <select id="grub-dev"
          style="padding:6px 10px;border:1px solid var(--line);border-radius:6px;background:var(--bg);color:var(--ink);font:inherit;font-family:var(--mono)">
          ${disks.map((k) => `<option value="${esc(k.path)}">${esc(k.path)} &mdash; ${esc(k.size)}${k.model ? " " + esc(k.model) : ""}</option>`).join("")}
        </select>
        ${disks.some((k) => (k.by_id || []).length) ? `<div class="msg" style="margin-top:8px">
          Stable names, for recognising the one grub had recorded:
          ${disks.map((k) => (k.by_id || []).map((n) =>
            `<div class="mono">${esc(k.path)} = ${esc(n)}</div>`).join("")).join("")}
        </div>` : ""}
      </div>`
    : "";

  return `<div class="card" style="border-color:var(--bad)">
    <h2>This machine is part-way through an upgrade</h2>
    <div class="step">
      <div class="note" style="border-color:var(--bad)">
        <b>dpkg stopped and will not do anything else until this is resolved.</b>
        ${mid.packages.length} package(s) are unpacked but not configured. Installing updates,
        applying the manifest and finishing the release upgrade all fail while this is true,
        and none of them will say why.
      </div>
      <ul style="margin:10px 0 0 18px">${mid.packages.map((k) => `<li class="mono">${esc(k)}</li>`).join("")}</ul>
    </div>
    ${picker}
    <div class="bar">
      <button class="act primary" ${connected && !busy ? "" : "disabled"}
        title="Runs dpkg --configure -a, then apt-get -f install, then continues the upgrade."
        onclick="finishUpgrade('${id}', ${mid.grub_stuck && disks.length ? "true" : "false"})">Finish the upgrade</button>
      <span class="status" id="finish-status"></span>
    </div>
  </div>`;
}

async function finishUpgrade(id, withDisk) {
  const dev = withDisk ? ($("grub-dev") || {}).value : null;
  if (!confirm(
    "Finish the interrupted upgrade?" +
    (dev ? "\n\nThe bootloader will be installed to " + dev + "." : "") +
    "\n\nThis configures what dpkg left unfinished and then carries on with the upgrade. " +
    "It can take a while, and the machine will need a reboot afterwards."
  )) return;

  const st = $("finish-status");
  const say = (t, c) => { if (st) st.innerHTML = `<span class="${c}">${esc(t)}</span>`; };
  say("working\u2026 this can take several minutes", "run");
  try {
    const res = await cmd(id, "finish_upgrade", dev ? { grub_device: dev } : {});
    if (!res || !res.id) throw new Error("the portal did not accept the command");
    const done = await awaitCommand(id, res.id, 3600000);
    if (!done) { say("still running \u2014 see the History tab", "run"); return; }
    say(done.ok ? "finished \u2014 reboot when convenient" : (done.summary || "still stuck"),
      done.ok ? "ok" : "bad");
  } catch (e) {
    say(e.message, "bad");
  }
}

// Whether this machine's host is backing it up.
//
// Only shown for a machine some hypervisor in the fleet claims as a guest.
// A backup existing is not a backup restoring, so the wording says what was
// observed - a file, written at a time - and does not promise more.
function backupCard(b) {
  if (!b || !b.host) return "";
  const age = b.last ? (Date.now() - new Date(b.last)) / 86400000 : null;
  const stale = age === null || age > 8;

  return `<div class="card"${stale ? ' style="border-color:var(--warn)"' : ""}>
    <h2>Backups</h2>
    <div class="step">
      ${kv([
        ["Host", esc(b.host)],
        ["Last backup", b.last
          ? `${ago(b.last)} <span class="msg">(${new Date(b.last).toLocaleString()})</span>`
          : '<span class="pill warn">none found</span>'],
      ])}
      ${stale ? `<div class="note" style="border-color:var(--warn);margin-top:10px">
        ${b.last
          ? "The most recent backup file for this machine is over a week old."
          : "No backup file for this machine was found on its host."}
        Anything below that cannot be undone &mdash; a release upgrade, a firmware write &mdash;
        has nothing to fall back on. A backup existing is still not proof it restores.
      </div>` : ""}
    </div>
  </div>`;
}

// What this machine hosts, or what hosts it.
//
// A hypervisor is the one machine whose patch state affects every other
// machine on it, and its guest list is where unmanaged machines show up -
// which is exactly what a fleet tool is otherwise blind to.
// Is backing up happening, and is anything stuck?
//
// That is the whole question. Not retention, not verification, not a second
// backup dashboard - just whether jobs are running, finishing, and finishing
// cleanly, plus a job that has been going long enough to be worth a look.
function backupState(b) {
  if (!b) return "";
  const stuck = (b.running || []).filter(
    (j) => (Date.now() - new Date(j.started)) / 3600000 >= 12);
  const failed = (b.recent || []).filter((j) => !j.ok);

  return `<div class="step">
    <h3>Backups${(b.running || []).length ? ` &mdash; ${b.running.length} running` : ""}</h3>
    ${b.note ? `<div class="note" style="border-color:var(--warn)">${esc(b.note)}</div>` : ""}
    ${stuck.length ? `<div class="note" style="border-color:var(--bad)">
      <b>${stuck.length} backup job(s) have been running for over 12 hours.</b>
      That is usually a job that is stuck rather than one that is slow.
      ${stuck.map((j) => `<div class="mono">${esc(j.guest || j.id)} &mdash; started ${ago(j.started)}</div>`).join("")}
    </div>` : ""}
    ${failed.length ? `<div class="note" style="border-color:var(--bad)">
      <b>${failed.length} of the last ${(b.recent || []).length} job(s) failed.</b>
      ${failed.slice(0, 3).map((j) => {
        // The first line of the log that looks like the actual complaint. A
        // status of "unable to create temporary directory" is the answer; the
        // eighty lines of transfer progress around it are not.
        const why = (j.log || "").split(/\r?\n/)
          .find((l) => /error|failed|cannot|unable|no space|permission/i.test(l));
        return `<div class="mono">${esc(j.guest || j.id)}: ${esc(j.status)}${
          why ? `<div class="msg">${esc(why.trim().slice(0, 160))}</div>` : ""}</div>`;
      }).join("")}
    </div>` : ""}
    ${(b.recent || []).length ? `<div class="scroll" style="max-height:200px;overflow:auto"><table>
      <tbody>${b.recent.map((j) => `<tr>
        <td class="mono">${esc(j.guest || "job")}</td>
        <td>${j.ok ? '<span class="pill ok">ok</span>' : `<span class="pill bad">${esc(j.status)}</span>`}</td>
        <td class="msg">${ago(j.started)}</td>
      </tr>${j.log ? `<tr><td colspan="3">
        <details><summary class="msg">what the job printed</summary>
          <pre style="max-height:220px">${esc(j.log)}</pre></details></td></tr>` : ""}`).join("")}</tbody></table></div>`
      : (b.note ? "" : `<div class="msg">No finished backup jobs recorded yet.</div>`)}
  </div>`;
}

function virtCard(virt) {
  if (!virt) return "";
  const guests = virt.guests || [];

  if (!guests.length) {
    // Being a guest is a single fact, not a card's worth of information.
    return `<div class="card"><h2>Virtualization</h2><div class="step">
      ${kv([["Role", esc(virt.role)], ["Platform", esc(virt.platform)]])}
      ${virt.note ? `<div class="msg">${esc(virt.note)}</div>` : ""}
    </div></div>`;
  }

  const running = guests.filter((g) => (g.state || "").toLowerCase().startsWith("running"));
  const unmanaged = guests.filter((g) => !g.managed);
  const unmanagedRunning = unmanaged.filter((g) =>
    (g.state || "").toLowerCase().startsWith("running"));

  return `<div class="card">
    <h2>Virtual machines &mdash; ${guests.length} on this host, ${running.length} running</h2>
    ${unmanagedRunning.length ? `<div class="step">
      <div class="note" style="border-color:var(--warn)">
        <b>${unmanagedRunning.length} running guest(s) have no agent.</b> PatchPanel cannot see
        what they are running or whether they are patched. They are counted here because a
        hypervisor is the one place the gap is visible at all &mdash; from anywhere else an
        unmanaged VM is simply invisible.
      </div>
    </div>` : ""}
    <div class="scroll scrolly"><table>
      <thead><tr><th>Guest</th><th>Id</th><th>Type</th><th>State</th><th>PatchPanel</th><th>Last backup</th></tr></thead>
      <tbody>${guests.map((g) => {
        const on = (g.state || "").toLowerCase().startsWith("running");
        return `<tr${on ? "" : ' style="opacity:.55"'}>
          <td>${esc(g.name)}</td>
          <td class="mono msg">${esc(g.id)}</td>
          <td class="mono msg">${esc(g.kind)}</td>
          <td>${esc(g.state) || "-"}</td>
          <td>${g.managed
            ? '<span class="pill ok">managed</span>'
            : (on ? '<span class="pill warn">no agent</span>' : '<span class="pill">no agent</span>')}</td>
          <td>${g.last_backup
            ? `${ago(g.last_backup)}${(Date.now() - new Date(g.last_backup)) / 86400000 > 8
                ? ' <span class="pill warn">stale</span>' : ""}`
            : '<span class="msg">none</span>'}</td>
        </tr>`;
      }).join("")}</tbody>
    </table></div>
    ${backupState(virt.backups)}
    <div class="bar"><span class="msg">${esc(virt.platform)} &middot;
      ${guests.filter((g) => g.managed).length} of ${guests.length} have an agent${
        virt.note ? ` &middot; ${esc(virt.note)}` : ""}</span></div>
  </div>`;
}

// What the machine's own logs say about its last restart.
//
// Only shown when it was not a clean one: a machine that shut down properly
// needs no explanation, and a card saying so on every page would be noise.
function bootCard(boot) {
  if (!boot || !boot.unexpected) return "";
  return `<div class="card" style="border-color:var(--warn)">
    <h2>This machine restarted on its own</h2>
    <div class="step">
      <div class="note" style="border-color:var(--warn)">
        <b>${esc(boot.summary)}.</b> The previous shutdown was never started &mdash; nothing
        asked this machine to stop. Below is what its log held from before it went, which is
        the only record of it and will be gone when the journal rotates.
      </div>
      ${boot.detail ? `<pre>${esc(boot.detail)}</pre>` : ""}
    </div>
  </div>`;
}

// Applications watched by version rather than managed by a package manager.
//
// A package manager answers "is anything newer in the repositories I am
// configured for", which is not the same question as "am I running an old
// version". Software from a vendor repository pinned to an old series reports
// nothing pending while the vendor is major versions ahead. PatchPanel does
// not update these - doing so would mean running vendor scripts as root - it
// just refuses to let them hide.
function trackedCard(tracked) {
  if (!tracked.length) return "";
  const behind = tracked.filter((t) => t.behind);

  return `<div class="card">
    <h2>Watched applications &mdash; ${tracked.length}${behind.length ? `, ${behind.length} behind` : ""}</h2>
    ${behind.length ? `<div class="step"><div class="note" style="border-color:var(--warn)">
      <b>${behind.length} application(s) are behind what the vendor publishes.</b>
      These do not appear in the pending count because no package manager offers them &mdash;
      the repository they came from carries an older series. PatchPanel will not update them;
      it only tells you that they are behind.
    </div></div>` : ""}
    <div class="scroll scrolly"><table>
      <thead><tr><th>Application</th><th>Installed</th><th>Published</th><th></th><th>Checked</th></tr></thead>
      <tbody>${tracked.map((t) => `<tr>
        <td>${esc(t.name)}<div class="mono msg">${esc(t.package)}</div>
          ${t.note ? `<div class="msg">${esc(t.note)}</div>` : ""}
          ${t.link ? `<div><a href="${esc(t.link)}" target="_blank" rel="noreferrer noopener" class="msg">vendor instructions</a></div>` : ""}</td>
        <td class="mono">${esc(t.installed)}</td>
        <td class="mono">${esc(t.latest) || '<span class="msg">unknown</span>'}</td>
        <td>${t.error
          ? `<span class="pill warn" title="${esc(t.error)}">could not check</span>`
          : (t.suspect
              ? `<span class="pill warn" title="The published version is older than the one installed, so this check is pointed at the wrong place. It is not evidence that the machine is current.">check looks wrong</span>`
              : (t.behind
                  ? '<span class="pill bad">behind</span>'
                  : (t.latest ? '<span class="pill ok">current</span>' : '<span class="pill">unknown</span>')))}</td>
        <td class="msg">${t.checked_at ? ago(t.checked_at) : "-"}</td>
      </tr>`).join("")}</tbody>
    </table></div>
  </div>`;
}

// Firmware is deliberately its own card, away from the update buttons.
//
// Everything else here can be undone: a package reinstalled, a source file
// rolled back, a release upgrade restored from a snapshot. A firmware write
// cannot, and a failed one can leave hardware that does not come back. So it
// is never part of a patch run and never installed by the manifest - it is
// reported, and flashed only when somebody asks for it by name.
function firmwareCard(list, id, connected, busy) {
  if (!(list || []).length) return "";
  const reboot = list.filter((f) => f.needs_reboot).length;

  return `<div class="card">
    <h2>Firmware &mdash; ${list.length} update(s) available</h2>
    <div class="step">
      <div class="note" style="border-color:var(--warn)">
        <b>Firmware is not installed by patch runs.</b> A package can be rolled back and
        firmware cannot: a write that goes wrong can leave the device unusable. Read the
        vendor's notes, make sure the machine will not lose power part-way, and flash it
        deliberately.
        ${reboot ? `<br><br>${reboot} of these are written now and applied at the next boot.
          Some need a full power cycle rather than a warm reboot.` : ""}
      </div>
    </div>
    <div class="scroll scrolly"><table>
      <thead><tr><th>Device</th><th>Installed</th><th>Available</th><th></th></tr></thead>
      <tbody>${list.map((f) => `<tr>
        <td>${esc(f.device)}${f.summary ? `<div class="msg">${esc(f.summary)}</div>` : ""}
          ${f.caution ? `<div class="msg" style="color:var(--warn)">${esc(f.caution)}</div>` : ""}</td>
        <td class="mono">${esc(f.current) || "-"}</td>
        <td class="mono">${esc(f.available)}</td>
        <td>${f.needs_reboot ? '<span class="pill">at next boot</span>' : ""}</td>
      </tr>`).join("")}</tbody>
    </table></div>
    <div class="bar">
      <button class="act" ${connected && !busy ? "" : "disabled"}
        title="Runs fwupdmgr update. This writes firmware to the devices listed above."
        onclick="updateFirmware('${id}')">Flash all firmware&hellip;</button>
      <span class="status" id="fw-status"></span>
    </div>
  </div>`;
}

async function updateFirmware(id) {
  const typed = prompt(
    "This writes firmware to this machine's hardware.\n\n" +
    "It cannot be undone, and a failure part-way through can leave a device unusable. " +
    "Make sure the machine will not lose power.\n\nType FLASH to continue:"
  );
  if (typed === null) return;
  if (typed.trim() !== "FLASH") {
    alert("Nothing was flashed.");
    return;
  }

  const st = $("fw-status");
  const say = (t, c) => { if (st) st.innerHTML = `<span class="${c}">${esc(t)}</span>`; };
  say("flashing \u2014 do not power this machine off", "run");
  try {
    const res = await cmd(id, "update_firmware", {});
    if (!res || !res.id) throw new Error("the portal did not accept the command");
    const done = await awaitCommand(id, res.id, 1800000);
    if (!done) { say("still running \u2014 see the History tab", "run"); return; }
    say(done.ok ? "written \u2014 reboot to apply it" : (done.summary || "failed"),
      done.ok ? "ok" : "bad");
  } catch (e) {
    say(e.message, "bad");
  }
}

function releaseCard(rel, id, connected, busy) {
  if (!rel) return "";
  const blockers = (rel.findings || []).filter((f) => f.severity === "blocker");
  const warnings = (rel.findings || []).filter((f) => f.severity === "warning");

  const verdict = blockers.length
    ? `<span class="pill bad">not safe to upgrade</span>`
    : (rel.next ? `<span class="pill ok">ready for ${esc(rel.next)}</span>`
                : `<span class="pill">nothing to upgrade to</span>`);

  const finding = (f) => `<div class="step">
      <h3>${f.severity === "blocker" ? '<span class="pill bad">blocker</span>' : '<span class="pill warn">warning</span>'}
        ${esc(f.summary)}</h3>
      ${f.detail ? `<pre>${esc(f.detail)}</pre>` : ""}
    </div>`;

  // The upgrade is offered in two steps on purpose. The check is free and
  // answers the only question worth asking beforehand; the upgrade itself
  // cannot be undone from here, so it asks for the release to be typed out.
  // Name the version, not just the codename. Someone reading "forky" has to
  // already know whether that is a release or what is currently in testing -
  // and if they assume the former, they upgrade a server onto testing.
  const target = rel.next_version
    ? `${esc(rel.distro)} ${esc(rel.next_version)} (${esc(rel.next)})`
    : `${esc(rel.distro)} ${esc(rel.next)}`;

  const upgrade = rel.next ? `<div class="step">
      <h3>Upgrade to ${target}${rel.stable === rel.next ? ' <span class="pill ok">current stable</span>' : ""}</h3>
      <p>PatchPanel finishes the current release, repoints apt at
         <span class="mono">${esc(rel.next)}</span> (keeping a copy of every source file it
         changes), disables third-party repositories that have nothing published for it, and
         runs the upgrade in the two stages Debian documents. It takes a while and the machine
         needs a reboot afterwards.</p>
      <div class="note" style="border-color:var(--warn)">
        <b>This cannot be undone from here.</b> Snapshot the machine first &mdash; on Proxmox,
        <span class="mono">Snapshots &rarr; Take Snapshot</span> &mdash; and read the readiness
        check before starting.
      </div>
      <div class="bar" style="padding-left:0;padding-right:0">
        <button class="act" ${connected && !busy ? "" : "disabled"}
          title="Runs every precondition and changes nothing."
          onclick="distroCheck('${id}', '${esc(rel.next)}')">Check readiness</button>
        <button class="act" ${connected && !busy && !blockers.length ? "" : "disabled"}
          title="${blockers.length ? "Resolve the blockers first" : "Performs the release upgrade"}"
          onclick="distroUpgrade('${id}', '${esc(rel.next)}', '${target.replace(/'/g, "")}')">Upgrade to ${target}&hellip;</button>
        <span class="status" id="distro-status"></span>
      </div>
      <pre id="distro-out" hidden></pre>
    </div>` : "";

  return `<div class="card">
    <h2>Release</h2>
    <div class="step">
      <div class="hdr" style="margin:0">
        <b>${esc(rel.distro)} ${esc(rel.version_id) || "testing"}</b>
        <span class="mono msg">${esc(rel.codename)}</span>
        ${verdict}
        ${rel.stable ? `<span class="msg">current stable is ${esc(rel.stable)}</span>` : ""}
      </div>
      ${blockers.length
        ? `<p style="margin-top:10px">Installing updates on this machine is unsafe until the
             blocker${blockers.length > 1 ? "s" : ""} below ${blockers.length > 1 ? "are" : "is"} resolved.</p>`
        : (rel.next ? `<p style="margin-top:10px">Next release is <b>${esc(rel.next)}</b>.</p>` : "")}
    </div>
    ${blockers.map(finding).join("")}
    ${warnings.map(finding).join("")}
    ${upgrade}
  </div>`;
}

// The readiness check and the upgrade share one reporting path: both are long
// enough that a button which merely dims tells you nothing.
async function runDistro(id, to, check) {
  const out = $("distro-out");
  const st = $("distro-status");
  const say = (text, cls) => {
    if (st) st.innerHTML = `<span class="${cls}">${esc(text)}</span>`;
  };
  say(check ? "checking\u2026" : "upgrading \u2014 this takes a while", "run");
  if (out) { out.hidden = true; out.textContent = ""; }

  let res;
  try {
    res = await cmd(id, "distro_upgrade", { to, check });
    if (!res || !res.id) throw new Error("the portal did not accept the command");
  } catch (e) {
    say(e.message, "bad");
    return;
  }

  // A release upgrade runs for the better part of an hour; the History tab is
  // where it keeps reporting once this page stops waiting.
  const done = await awaitCommand(id, res.id, check ? 120000 : 3600000);
  if (!done) {
    say("still running \u2014 see the History tab (" + res.id.slice(0, 8) + ")", "run");
    return;
  }
  say(done.ok ? (check ? "check complete" : "upgrade complete \u2014 reboot when convenient")
              : (check ? "not ready" : "upgrade failed"), done.ok ? "ok" : "bad");
  if (out) {
    out.hidden = false;
    out.textContent = done.detail || done.summary || "";
  }
}

function distroCheck(id, to) { runDistro(id, to, true); }

// Rebooting is scheduled a minute out rather than immediately: the agent gets
// its reply to the portal before the machine goes, so the action does not read
// as failed, and anyone logged in gets the usual warning with time to object.
const REBOOT_DELAY_SECS = 60;

async function rebootMachine(id, host) {
  if (!confirm(
    "Reboot " + host + " in one minute?\n\n" +
    "Anything running on it stops. It should reconnect to PatchPanel by itself " +
    "once it is back."
  )) return;

  const say = (text, cls) => {
    for (const el of [$("reboot-status"), $("reboot-status-2")]) {
      if (el) el.innerHTML = `<span class="${cls}">${esc(text)}</span>`;
    }
  };
  say("scheduling\u2026", "run");
  try {
    const res = await cmd(id, "reboot", { delay_secs: REBOOT_DELAY_SECS });
    if (!res || !res.id) throw new Error("the portal did not accept the command");
    say("rebooting in " + REBOOT_DELAY_SECS + "s \u2014 it will show as offline, then come back",
      "ok");
  } catch (e) {
    say(e.message, "bad");
  }
}

function distroUpgrade(id, to, label) {
  label = label || to;
  // Typing the release is the last gate. A misclick cannot get through it, and
  // the words are the ones the operator has to have read.
  const typed = prompt(
    "This upgrades the machine to " + label + " and cannot be undone from PatchPanel.\n\n" +
    "Snapshot it first.\n\nType " + to + " to continue:"
  );
  if (typed === null) return;
  if (typed.trim() !== to) {
    alert("Not upgraded - you typed \"" + typed + "\", which is not " + to + ".");
    return;
  }
  runDistro(id, to, false);
}

// Two source lines that differ only in spacing say the same thing to apt, and
// showing them as a change buries the one line that actually moved.
const sameLine = (a, b) =>
  (a ?? "").trim().replace(/\s+/g, " ") === (b ?? "").trim().replace(/\s+/g, " ");

function lineDiff(before, after) {
  const A = before.replace(/\s+$/, "").split("\n");
  const B = after.replace(/\s+$/, "").split("\n");
  const rows = [];
  for (let i = 0; i < Math.max(A.length, B.length); i++) {
    const x = A[i], y = B[i];
    if (sameLine(x, y)) { rows.push({ t: "same", s: y ?? x ?? "" }); continue; }
    if (x !== undefined && x.trim() !== "") rows.push({ t: "del", s: x });
    if (y !== undefined && y.trim() !== "") rows.push({ t: "add", s: y });
  }
  return rows;
}

const sameText = (a, b) => {
  const A = (a ?? "").replace(/\s+$/, "").split("\n");
  const B = (b ?? "").replace(/\s+$/, "").split("\n");
  return A.length === B.length && A.every((l, i) => sameLine(l, B[i]));
};

function renderDiff(before, after) {
  const rows = lineDiff(before, after);
  const changed = rows.filter((r) => r.t !== "same").length;
  if (!changed) return `<div class="msg">No differences.</div>`;
  return `<div class="diff">${rows.map((r) =>
    `<div class="${r.t}">${r.t === "del" ? "- " : r.t === "add" ? "+ " : "  "}${esc(r.s)}</div>`
  ).join("")}</div>`;
}

function sourceEditor(files, id, connected) {
  if (!files.length) return "";

  const block = (f, i) => {
    const notes = (f.notes || []).length
      ? `<ul style="margin:6px 0 10px 18px">${f.notes.map((n) => `<li>${esc(n)}</li>`).join("")}</ul>`
      : "";
    const suggestion = f.suggested
      ? `<div class="note" style="border-color:var(--accent)">
           <b>PatchPanel can correct this file.</b>
           ${notes}
           <div id="sugdiff-${i}">${renderDiff(f.content, f.suggested)}</div>
           <button class="act" onclick="useSuggestion('${i}')">Apply this to the editor</button>
           <span class="msg">nothing is written until you press Save</span>
           <textarea id="sug-${i}" hidden>${esc(f.suggested)}</textarea>
         </div>`
      : "";
    return `
      <div class="step">
        <h3 class="mono">${esc(f.path)}${f.suggested ? ' <span class="pill warn">fix available</span>' : ""}</h3>
        ${suggestion}
        <textarea id="src-${i}" spellcheck="false" style="min-height:130px"
          data-original="${esc(f.content)}" data-dirty="0" data-path="${esc(f.path)}"
          oninput="onEdit(${i})">${esc(f.content)}</textarea>
        <div id="pending-${i}" hidden>
          <div class="msg" style="margin-top:8px">Unsaved changes &mdash; this is what Save will write:</div>
          <div id="editdiff-${i}"></div>
        </div>
        <div class="bar" style="padding-left:0;padding-right:0">
          <button class="act primary" ${connected ? "" : "disabled"} id="save-${i}"
            onclick="saveSource('${id}', ${i})">Save and validate</button>
          <button class="act" onclick="revertSource(${i})">Revert</button>
          <button class="act" ${connected ? "" : "disabled"}
            onclick="deleteSource('${id}', ${i})">Delete file</button>
          <span class="status" id="status-${i}">${statusHTML(f.path)}</span>
        </div>
      </div>`;
  };

  const fixable = files.filter((f) => f.suggested).length;
  return `<div class="card">
    <h2>Apt sources${fixable ? ` &mdash; ${fixable} file(s) with a suggested fix` : ""}</h2>
    <div class="step"><div class="note">
      Saving runs <code>apt-get update</code> on the machine. If apt rejects the new contents the
      agent puts the previous file back and tells you why, so a bad edit undoes itself. A copy is
      kept beside each file as <code>.patchpanel-bak</code>.
    </div></div>
    ${files.map(block).join("")}
  </div>`;
}

// Show what the current editor content would change, so "apply the fix" is
// never a leap of faith.
function onEdit(i) {
  const ta = document.getElementById("src-" + i);
  // Retyping the same line with different spacing is not an edit worth
  // warning about, or worth writing to the machine.
  const dirty = !sameText(ta.value, ta.dataset.original);
  ta.dataset.dirty = dirty ? "1" : "0";

  const pending = document.getElementById("pending-" + i);
  if (pending) {
    pending.hidden = !dirty;
    if (dirty) {
      document.getElementById("editdiff-" + i).innerHTML =
        renderDiff(ta.dataset.original, ta.value);
    }
  }
  const banner = document.getElementById("edit-banner");
  if (banner) banner.hidden = !isEditing();
}

function useSuggestion(i) {
  const ta = document.getElementById("src-" + i);
  const sug = document.getElementById("sug-" + i);
  if (!ta || !sug) return;
  ta.value = sug.value;
  onEdit(i);
  ta.scrollIntoView({ block: "nearest" });
}

function revertSource(i) {
  const ta = document.getElementById("src-" + i);
  if (!ta) return;
  ta.value = ta.dataset.original;
  onEdit(i);
  setStatus(i, "", "", ta.dataset.path);
}

// Shared row filter for the long tables.
// winget cannot always determine what is installed; it prints `Unknown`, or a
// bound like `< 4.6.2`. Those are the packages that quietly never upgrade.
function unreadableVersion(u) {
  if (u.source !== "winget") return false;
  const v = (u.current_version || "").trim();
  return v === "" || v === "Unknown" || v.startsWith("<");
}

function filterRows(inputId, tbodyId) {
  const q = (document.getElementById(inputId)?.value || "").toLowerCase();
  document.querySelectorAll(`#${tbodyId} tr`).forEach((tr) => {
    tr.hidden = q && !(tr.dataset.n || "").includes(q);
  });
}

// Last save outcome per file path, so it survives the refresh timer.
const SAVED = new Map();

function statusHTML(path) {
  const st = SAVED.get(path);
  if (!st) return "";
  return `<span class="${st.cls}">${esc(st.text)}</span>`;
}

function setStatus(i, text, cls, path) {
  const el = document.getElementById("status-" + i);
  if (el) el.innerHTML = text ? `<span class="${cls}">${esc(text)}</span>` : "";
  const key = path || pathOf(i);
  if (!key) return;
  if (text) SAVED.set(key, { text, cls });
  else SAVED.delete(key);
}

function pathOf(i) {
  const ta = document.getElementById("src-" + i);
  return ta ? ta.dataset.path : null;
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// Wait for the agent to report on a dispatched command, so Save says what
// actually happened instead of silently returning.
async function awaitCommand(agentId, cmdId, timeoutMs = 90000) {
  const until = Date.now() + timeoutMs;
  while (Date.now() < until) {
    await sleep(1500);
    try {
      const rows = await api(`/api/commands?agent=${agentId}&limit=15`);
      const c = rows.find((r) => r.id === cmdId);
      if (c && c.ok !== null && c.ok !== undefined) return c;
    } catch (e) {
      // Keep waiting; a transient failure to poll is not a failed command.
    }
  }
  return null;
}

async function saveSource(id, i) {
  const ta = document.getElementById("src-" + i);
  const path = ta.dataset.path;
  const content = ta.value;
  if (!confirm("Replace " + path + " on this machine? apt validates it, and the old file is restored if it is rejected.")) return;

  const btn = document.getElementById("save-" + i);
  if (btn) btn.disabled = true;
  setStatus(i, "writing " + path + " and running apt-get update\u2026", "run", path);

  let res;
  try {
    res = await cmd(id, "write_source", { path, content });
    if (!res || !res.id) throw new Error("the portal did not accept the command");
  } catch (e) {
    setStatus(i, e.message, "bad", path);
    if (btn) btn.disabled = false;
    return;
  }

  const done = await awaitCommand(id, res.id);
  if (btn) btn.disabled = false;

  if (!done) {
    setStatus(i, "still running \u2014 see the History tab (" + res.id.slice(0, 8) + ")", "run", path);
    return;
  }
  if (done.ok) {
    // Only now does the editor match what is on disk.
    ta.dataset.original = content;
    onEdit(i);
    setStatus(i, "saved \u2014 " + (done.summary || "apt accepted it"), "ok", path);
  } else {
    setStatus(i, "not saved \u2014 " + (done.summary || "apt rejected it, the previous file was restored"), "bad", path);
  }
}

async function deleteSource(id, i) {
  const path = pathOf(i);
  if (!confirm("Delete " + path + "? A backup is kept beside it.")) return;
  setStatus(i, "deleting " + path + "\u2026", "run", path);
  try {
    const res = await cmd(id, "remove_source", { path });
    if (!res || !res.id) throw new Error("the portal did not accept the command");
    const done = await awaitCommand(id, res.id);
    if (!done) { setStatus(i, "still running \u2014 see the History tab", "run", path); return; }
    setStatus(i, (done.ok ? "deleted \u2014 " : "not deleted \u2014 ") + (done.summary || ""),
      done.ok ? "ok" : "bad", path);
  } catch (e) {
    setStatus(i, e.message, "bad", path);
  }
}

// Side-by-side apt sources for two machines.
//
// The fleet-wide diff answers "is this machine unusual"; this answers "why is
// that one working and this one not", which is the question actually being
// asked when two boxes of the same release behave differently.
function compareCard(me, myRepos, myFiles) {
  const others = AGENTS.filter((a) => a.id !== me.id);
  const label = (a) =>
    `${a.hostname}${a.os_version ? ` - ${a.os_version}` : ""}`;
  // Same release first: those are the comparisons where a difference means
  // something.
  const mine = me.os_version || "";
  others.sort((a, b) => {
    const sa = (a.os_version === mine ? 0 : 1), sb = (b.os_version === mine ? 0 : 1);
    return sa - sb || a.hostname.localeCompare(b.hostname);
  });

  const picker = `<div class="step">
      <label class="msg">Compare apt sources with</label>
      <select id="cmp-with" onchange="setCompare(this.value)"
        style="margin-left:8px;padding:6px 10px;border:1px solid var(--line);border-radius:6px;background:var(--bg);color:var(--ink);font:inherit">
        <option value="">nothing</option>
        ${others.map((a) => `<option value="${a.id}"${a.id === COMPARE ? " selected" : ""}>${esc(label(a))}</option>`).join("")}
      </select>
      ${COMPARE && COMPARE_DATA ? `<button class="act" style="margin-left:8px" onclick="setCompare('${COMPARE}')">Refresh</button>` : ""}
    </div>`;

  if (!COMPARE || !COMPARE_DATA) {
    return `<div class="card"><h2>Compare with another machine</h2>${picker}</div>`;
  }

  const them = COMPARE_DATA;
  const theirInv = them.inventory || {};
  const theirRepos = (theirInv.repositories || []).filter((r) => r.enabled);
  const myEnabled = myRepos.filter((r) => r.enabled);
  const key = (r) => `${r.source} ${(r.uri || "").replace(/\/$/, "")} ${r.suite || ""}`;
  // What to show for a key: an apt repository is written as a `deb` line, and
  // showing anything else invites it being pasted into a source file. The
  // backend name belongs in a column of its own, not at the front of a line
  // that otherwise looks copyable.
  const shown = (k) => {
    const [backend, ...rest] = k.split(" ");
    return backend === "apt" || backend === "apt-src"
      ? `${backend === "apt-src" ? "deb-src" : "deb"} ${rest.join(" ").trim()}`
      : k;
  };

  const mineSet = new Set(myEnabled.map(key));
  const theirSet = new Set(theirRepos.map(key));
  const all = [...new Set([...mineSet, ...theirSet])].sort();
  const both = all.filter((k) => mineSet.has(k) && theirSet.has(k)).length;

  const rows = all.map((k) => {
    const a = mineSet.has(k), b = theirSet.has(k);
    const mark = (on) => on
      ? '<span class="pill ok">yes</span>'
      : '<span class="pill bad">no</span>';
    return `<tr${a && b ? ' style="opacity:.55"' : ""}>
      <td class="mono">${esc(shown(k))}</td>
      <td>${mark(a)}</td>
      <td>${mark(b)}</td>
    </tr>`;
  }).join("");

  // Files with the same path on both machines get a real diff, which is what
  // you want once the table has told you which repo is missing.
  const theirFiles = theirInv.source_files || [];
  const shared = (myFiles || []).filter((f) => theirFiles.some((g) => g.path === f.path));
  const diffs = shared.map((f) => {
    const g = theirFiles.find((x) => x.path === f.path);
    const same = sameText(f.content, g.content);
    return `<div class="step">
      <h3 class="mono">${esc(f.path)} ${same ? '<span class="pill ok">identical</span>' : '<span class="pill warn">differs</span>'}</h3>
      ${same ? "" : renderDiff(g.content, f.content)}
    </div>`;
  }).join("");

  const onlyMine = (myFiles || []).filter((f) => !theirFiles.some((g) => g.path === f.path));
  const onlyTheirs = theirFiles.filter((g) => !(myFiles || []).some((f) => f.path === g.path));

  const sameRelease = them.os_version === me.os_version;
  return `<div class="card">
    <h2>Compared with ${esc(them.hostname)}</h2>
    ${picker}
    <div class="step">
      ${sameRelease
        ? `<div class="msg">Both machines run ${esc(me.os_version)}, so a difference here is a real one.</div>`
        : `<div class="note" style="border-color:var(--warn)">
             <b>Different releases.</b> ${esc(me.hostname)} runs ${esc(me.os_version)} and
             ${esc(them.hostname)} runs ${esc(them.os_version)}. Their archives are
             <i>supposed</i> to differ; read this as a shape comparison, not a checklist.
           </div>`}
    </div>
    <div class="scroll scrolly"><table>
      <thead><tr><th>Enabled repository</th><th>${esc(me.hostname)}</th><th>${esc(them.hostname)}</th></tr></thead>
      <tbody>${rows}</tbody>
    </table></div>
    <div class="bar"><span class="msg">${both} in common, ${all.length - both} on only one of them</span></div>
    ${diffs ? `<h3 style="padding:0 14px">Files on both machines</h3>${diffs}` : ""}
    ${onlyMine.length ? `<div class="step"><div class="msg">Only on ${esc(me.hostname)}:
      ${onlyMine.map((f) => `<span class="mono">${esc(f.path)}</span>`).join(", ")}</div></div>` : ""}
    ${onlyTheirs.length ? `<div class="step"><div class="msg">Only on ${esc(them.hostname)}:
      ${onlyTheirs.map((f) => `<span class="mono">${esc(f.path)}</span>`).join(", ")}</div></div>` : ""}
  </div>`;
}

// A repository key rendered the way it is written in a sources file. The
// internal form starts with the backend name, which reads as a source type and
// is not one.
function asSourceLine(k) {
  const [backend, ...rest] = String(k).split(" ");
  if (backend === "apt") return `deb ${rest.join(" ").trim()}`;
  if (backend === "apt-src") return `deb-src ${rest.join(" ").trim()}`;
  return k;
}

function repoCard(repos, diff) {
  if (!repos.length) return "";
  const odd = new Set(diff.only_here || []);
  const key = (r) => `${r.source} ${(r.uri || "").replace(/\/$/, "")} ${r.suite || ""}`;

  const rows = repos.map((r) => {
    const flags = [];
    if (!r.enabled) flags.push('<span class="pill">disabled</span>');
    if (odd.has(key(r))) flags.push('<span class="pill warn">only on this machine</span>');
    if (r.problem) flags.push(`<span class="pill bad" title="${esc(r.problem)}">cannot deliver</span>`);
    return `<tr${r.enabled ? "" : ' style="opacity:.55"'}>
      <td class="mono">${esc(r.source)}</td>
      <td class="mono">${esc(r.uri)}</td>
      <td class="mono">${esc(r.suite) || "-"}</td>
      <td class="mono msg">${esc((r.components || []).join(" "))}</td>
      <td>${flags.join(" ")}</td>
      <td class="mono msg">${esc(r.origin_file)}</td>
    </tr>`;
  }).join("");

  // A repository whose files have gone is the reason a patch run downloads
  // hundreds of megabytes and then fails, and nothing about the source line
  // itself looks wrong. Say it above the table, not in a tooltip.
  const broken = repos.filter((r) => r.problem);
  const brokenNote = broken.length
    ? `<div class="step"><div class="note" style="border-color:var(--bad)">
         <b>${broken.length} source(s) list packages that are no longer served.</b>
         ${broken.map((r) => `<div style="margin-top:8px">
           <div class="mono">${esc(r.uri)} ${esc(r.suite)}</div>
           <div class="msg">${esc(r.problem)}</div>
           <div class="msg">declared in <span class="mono">${esc(r.origin_file)}</span></div>
         </div>`).join("")}
       </div></div>`
    : "";

  const missing = (diff.missing_here || []).length
    ? `<div class="note" style="margin:12px 14px">
         <b>Missing here.</b> Configured on all ${diff.peers} other ${esc(diff.group || "machine")} machine(s), but not this one:
         <ul style="margin:6px 0 0 18px">${diff.missing_here.map((m) => `<li class="mono">${esc(asSourceLine(m))}</li>`).join("")}</ul>
       </div>`
    : "";

  // Say what the comparison was against. "The same OS" was a lie that made a
  // Debian 12 box look wrong next to a Debian 13 one.
  const summary = diff.peers
    ? `compared against ${diff.peers} other machine(s) running ${esc(diff.group || "the same OS")}`
    : `no other machines running ${esc(diff.group || "this OS")} to compare against`;

  return `<div class="card">
    <h2>Package sources &mdash; ${repos.length}${broken.length ? `, ${broken.length} not serving` : ""}</h2>
    ${brokenNote}
    <div class="scroll scrolly"><table>
      <thead><tr><th>Backend</th><th>URI</th><th>Suite</th><th>Components</th><th></th><th>Declared in</th></tr></thead>
      <tbody>${rows}</tbody>
    </table></div>
    ${missing}
    <div class="bar"><span class="msg">${summary}</span></div>
  </div>`;
}

async function loadAgent(id) {
  const body = $("agent-body");
  if (!id) { body.innerHTML = `<div class="empty">No machine selected.</div>`; return; }

  let d;
  try {
    d = await api(`/api/agents/${id}`);
  } catch (e) {
    body.innerHTML = `<div class="back" onclick="showTab('fleet')">&larr; Fleet</div>
      <div class="card"><div class="empty">${esc(e.message)}</div></div>`;
    return;
  }

  const hw = d.hardware || {};
  const inv = d.inventory || {};
  const updates = inv.updates || [];
  const sec = updates.filter((u) => u.security);
  // Separate what a button press would actually install from what the archive
  // is withholding, so the page never implies there is work to do when there
  // is not.
  const stuckNames = new Set(inv.deferred || []);
  // Observed, not reported: these survived a patch run untouched.
  const blockedNames = new Set(inv.blocked || []);
  const canInstall = updates.filter((u) => !stuckNames.has(u.name));
  const phased = updates.filter((u) => stuckNames.has(u.name));
  const busy = isBusy(d);
  const state = d.connected
    ? '<span class="pill ok">connected</span>'
    : (d.online ? '<span class="pill warn">recently seen</span>' : '<span class="pill bad">offline</span>');

  // Patch history is the command log filtered to the actions that change the
  // machine; a "rescan" is not an event anyone wants in a history.
  const CHANGED = ["apply_patches", "apply_manifest", "self_update", "reboot", "distro_upgrade",
    "finish_upgrade", "update_firmware"];
  const history = (d.commands || []).filter((c) => HISTORY_ALL || CHANGED.includes(c.kind));
  const hidden = (d.commands || []).length - history.length;

  // Never redraw over unsaved edits.
  if (isEditing()) return;

  const files = inv.source_files || [];
  const fixable = files.filter((f) => f.suggested).length;
  const repos = inv.repositories || [];
  const drift = inv.drift || [];
  const issues = inv.scan_issues || [];
  const held = inv.held_back || [];
  const blocked = issues.length + held.length;

  // Which panes this machine has. Windows has no apt sources, so it gets no
  // Sources tab rather than an empty one.
  const panes = [
    { p: "overview", label: "Overview" },
    { p: "updates", label: "Updates",
      n: (canInstall.length + (inv.firmware || []).length) || null, warn: blocked > 0 },
  ];
  if (files.length || repos.length) {
    panes.push({ p: "sources", label: "Sources", n: fixable || null, warn: fixable > 0 });
  }
  panes.push({ p: "packages", label: "Packages", n: (inv.packages || []).length || null });
  panes.push({ p: "history", label: "History", n: history.length || null });

  const subnav = `<div class="subnav">${panes.map((t) =>
    `<button data-p="${t.p}" onclick="showPane('${t.p}')">${t.label}${
      t.n ? `<span class="n${t.warn ? " warn" : ""}">${t.n}</span>` : ""}</button>`).join("")}</div>`;

  const html = `
    <div class="machine-head">
      <div class="back" onclick="showTab('fleet')">&larr; Fleet</div>
      <div class="hdr">
        <h2>${esc(d.hostname)}</h2>${state}
        ${busy ? `<span class="pill busy">${esc(KIND_LABEL[busy] || busy)}</span>` : ""}
        ${d.reboot_required ? '<span class="pill warn">reboot required</span>' : ""}
        ${/ (testing|unstable) /.test(" " + d.os_version + " ")
          ? `<span class="pill warn" title="This machine tracks a rolling suite rather than a released version. It has no version number and no security team of its own.">${esc(d.os_version.includes("unstable") ? "unstable" : "testing")}</span>`
          : ""}
        <span class="msg">${esc(d.os_version)} &middot; ${esc(d.site) || "no site"}</span>
      </div>
      ${subnav}
    </div>
    <div id="edit-banner" hidden>Editing &mdash; live updates are paused for this page until you save or revert.</div>

    <div data-pane="overview" hidden>
      ${midUpgradeCard(inv.mid_upgrade, d.id, d.connected, busy)}

      ${d.reboot_required ? `<div class="card"><div class="step">
        <div class="note" style="border-color:var(--warn)">
          <b>This machine is waiting on a reboot.</b> Updates have been installed that are not
          in effect until it restarts &mdash; on Linux that is usually a kernel or libc, which
          means the running system is still the unpatched one.
        </div>
        <div class="bar" style="padding-left:0;padding-right:0">
          <button class="act primary" ${d.connected && !busy ? "" : "disabled"}
            onclick="rebootMachine('${d.id}', '${esc(d.hostname)}')">Reboot ${esc(d.hostname)}&hellip;</button>
          <span class="status" id="reboot-status-2"></span>
        </div>
      </div></div>` : ""}

      <div class="card">
        <h2>Forward this machine's logs <span class="sub">off unless you turn it on</span></h2>
        <div class="step">
          <div class="msg">An inventory scan runs hourly and reads package state. A ZFS
            checksum error, an OOM kill, a machine-check exception or a unit crash-looping
            happens between scans and leaves nothing in a package list behind it. Forwarding
            reads this machine's journal and sends <b>warnings and worse</b> over the
            connection the agent already has, where a rolling day is kept.
            <b>Nothing is installed and nothing is written to the machine</b> &mdash; no
            rsyslog, no config file, no extra port. Stopping it ends a task.</div>
          ${(inv.syslog_forward || null)
            ? `<div class="bar" style="margin-top:10px">
                 <span class="pill ok">forwarding ${esc(inv.syslog_forward.min_severity)} and worse</span>
                 <span class="msg">to <span class="mono">${esc(inv.syslog_forward.target)}</span>
                   via <span class="mono">${esc(inv.syslog_forward.path)}</span></span>
                 <button class="act" ${d.connected && !busy ? "" : "disabled"} style="margin-left:auto"
                   title="Count what this machine's journal holds at each level, over the last day."
                   onclick="estimateVolume('${id}')">Estimate volume</button>
                 <button class="act" ${d.connected && !busy ? "" : "disabled"}
                   onclick="setForwarding('${id}', false)">Stop forwarding</button>
               </div>`
            : `<div class="bar" style="margin-top:10px">
                 <span class="msg">Not forwarding.</span>
                 <select id="fwd-sev" class="fld" style="margin-left:auto">
                   <option value="warning" selected>warnings and worse</option>
                   <option value="err">errors and worse</option>
                   <option value="notice">notices and worse</option>
                   <option value="info">everything (noisy)</option>
                 </select>
                 <button class="act" ${d.connected && !busy ? "" : "disabled"}
                   title="Count what this machine's journal holds at each level, over the last day. Reads nothing out to the portal."
                   onclick="estimateVolume('${id}')">Estimate volume</button>
                 <button class="act" ${d.connected && !busy ? "" : "disabled"}
                   onclick="setForwarding('${id}', true)">Start forwarding</button>
               </div>`}
          <div id="vol-out" class="msg" style="margin-top:8px"></div>
        </div>
      </div>

      <div class="grid2">
        <div class="card">
          <h2>Hardware</h2>
          ${kv([
            ["CPU", esc(hw.cpu_model) || "-"],
            ["Cores", hw.cpu_threads ? `${hw.cpu_cores || "?"} physical / ${hw.cpu_threads} logical` : "-"],
            ["Memory", hw.memory_mb ? `${(hw.memory_mb / 1024).toFixed(1)} GB` : "-"],
            ["Architecture", `<span class="mono">${esc(d.arch)}</span>`],
            ["System", esc(hw.vendor)],
            ["IP", (hw.ip_addresses || []).map((i) => `<span class="mono">${esc(i)}</span>`).join("<br>") || "-"],
            ["Kernel", `<span class="mono">${esc(hw.kernel)}</span>`],
          ])}
        </div>

        <div class="card">
          <h2>State</h2>
          ${kv([
            ["Booted", d.boot_time
            ? `${since(d.boot_time)} ago <span class="msg">(${new Date(d.boot_time).toLocaleString()})</span>${
                inv.boot && inv.boot.unexpected
                  ? ` <span class="pill bad" title="${esc(inv.boot.summary)}">did not shut down cleanly</span>`
                  : (inv.boot ? ' <span class="pill ok">clean shutdown</span>' : "")}`
            : "-"],
            ["Last seen", `${ago(d.last_seen)}`],
            ["First enrolled", new Date(d.first_seen).toLocaleString()],
            ["Agent version", `<span class="mono">${esc(d.agent_version)}</span>`],
            ["Manifest", d.applied_revision < REV
              ? `<span class="pill warn">r${d.applied_revision}</span> portal is at r${REV}`
              : `r${d.applied_revision} <span class="msg">up to date</span>`],
            ["Package backends", (d.backends || []).map((b) => `<span class="mono">${esc(b)}</span>`).join(", ")],
            ["Agent id", `<span class="mono msg">${esc(d.id)}</span>`],
          ])}
          <div class="bar">
            <button class="act" ${d.connected && !busy ? "" : "disabled"}
              title="Restarts the agent process. Re-detects package backends; does not touch the machine otherwise."
              onclick="restartAgent('${d.id}')">Restart agent</button>
            <button class="act" ${d.connected && !busy ? "" : "disabled"}
              title="Reboots the machine itself, one minute from now."
              onclick="rebootMachine('${d.id}', '${esc(d.hostname)}')">Reboot machine&hellip;</button>
            <span class="status" id="reboot-status"></span>
          </div>
        </div>
      </div>

      ${backupCard(d.backup)}

      ${virtCard(inv.virt)}

      ${bootCard(inv.boot)}

      ${releaseCard(inv.release, d.id, d.connected, busy)}
    </div>

    <div data-pane="updates" hidden>
      <div class="card">
        <h2>Pending updates &mdash; ${canInstall.length} to install${phased.length ? `, ${phased.length} phased` : ""}${blockedNames.size ? `, ${blockedNames.size} blocked` : ""}${sec.length ? `, ${sec.length} security` : ""}${(d.ignored || []).length ? `, ${d.ignored.length} ignored` : ""}</h2>
      ${blockedNames.size ? `<div class="step"><div class="note" style="border-color:var(--bad)">
        <b>${blockedNames.size} update(s) survived the last patch run untouched.</b>
        They were attempted, nothing was installed, and they are still offered at the same
        version &mdash; so pressing Install updates again will do exactly the same thing.
        Each has to be dealt with individually: on Windows that usually means the package
        manager will not replace it in place and it has to be uninstalled and reinstalled;
        on Linux it means apt could not resolve it. The last patch run in the History tab
        names them with whatever the tool said.
      </div></div>` : ""}
        ${phased.length && !canInstall.length ? `<div class="step"><div class="note">
          <b>Nothing to install.</b> All ${phased.length} pending update(s) are phased: the archive is
          withholding them from this machine until the rollout completes. Pressing Install updates
          will correctly do nothing. They will arrive on their own.
        </div></div>` : ""}
        ${updates.length > 12 ? `<div class="step" style="padding-bottom:0">
          <input id="upd-filter" placeholder="filter packages&hellip;" spellcheck="false"
            oninput="filterRows('upd-filter','upd-rows')"
            style="width:100%;padding:7px 10px;border:1px solid var(--line);border-radius:6px;background:var(--bg);color:var(--ink);font-family:var(--mono);font-size:12.5px">
        </div>` : ""}
        ${updates.length ? `<div class="scroll scrolly"><table>
          <thead><tr><th>Package</th><th>Installed</th><th>Available</th><th>Source</th><th></th></tr></thead>
          <tbody id="upd-rows">${canInstall.concat(phased).map((u) => {
            const stuck = stuckNames.has(u.name);
            return `<tr data-n="${esc((u.name || "").toLowerCase())}"${stuck ? ' style="opacity:.55"' : ""}>
            <td>${esc(u.name)}${u.security ? ' <span class="pill bad">security</span>' : ""}</td>
            <td class="mono"${unreadableVersion(u) ? ' title="winget could not read the installed version, only bound it. That on its own does not stop an upgrade."' : ""}>${esc(u.current_version) || "-"}</td>
            <td class="mono">${esc(u.new_version)}</td>
            <td class="mono">${esc(u.source)}</td>
            <td>${stuck ? '<span class="pill" title="Phased: the archive is withholding this from this machine. Nothing to do.">phased</span>' : ""}${
            blockedNames.has(u.name) ? ' <span class="pill bad" title="A patch run installed everything it could and left this exactly as it was - same version installed, same version on offer. Pressing Install updates again will not change it.">blocked</span>' : ""}
            <button class="act" style="padding:2px 8px;font-size:11.5px"
              title="Stop showing this version. It comes back on its own if a newer one is published."
              onclick="ignoreUpdate(${jsq(d.id)}, ${jsq(u.name)}, ${jsq(u.source)}, ${jsq(u.new_version)})">Ignore</button>
          </td></tr>`;
          }).join("")}</tbody>
        </table></div>` : `<div class="empty">Nothing pending${inv.collected_at ? ` as of ${ago(inv.collected_at)}` : ""}.</div>`}
        <div class="bar">
          <button class="act" ${busy ? "disabled" : (d.connected ? "" : "disabled")} onclick="cmd('${d.id}','collect_inventory')">Rescan</button>
          <button class="act" ${busy || !canInstall.length ? "disabled" : (d.connected ? "" : "disabled")}
            title="${canInstall.length ? "Install the pending updates" : "Nothing to install - everything pending is phased"}"
            onclick="patchNow('${d.id}','${esc(d.hostname)}')">Install updates</button>
          <button class="act" ${busy ? "disabled" : (d.connected ? "" : "disabled")} onclick="cmd('${d.id}','apply_manifest')">Apply manifest</button>
          <span class="msg">${busy
            ? `waiting for ${esc(KIND_LABEL[busy] || busy)} to finish`
            : (inv.collected_at ? `inventory collected ${ago(inv.collected_at)}` : "")}</span>
        </div>
      </div>

      ${trackedCard(d.tracked || [])}

      ${firmwareCard(inv.firmware, d.id, d.connected, busy)}

      ${ignoredCard(d.ignored || [], d.id)}

      ${scanCard(issues, held, inv.deferred || [], d.id, d.connected)}

      ${drift.length ? `<div class="card"><h2>Application drift</h2>
        <div class="scroll"><table><thead><tr><th>App</th><th>Wanted</th><th>Found</th></tr></thead>
        <tbody>${drift.map((x) => `<tr><td>${esc(x.app)}</td>
          <td class="mono">${esc(x.desired)}</td><td class="mono">${esc(x.observed)}</td></tr>`).join("")}</tbody>
        </table></div></div>` : ""}
    </div>

    ${files.length || repos.length ? `<div data-pane="sources" hidden>
      ${sourceEditor(files, d.id, d.connected)}
      ${repoCard(repos, d.repo_diff || {})}
      ${compareCard(d, repos, files)}
    </div>` : ""}

    <div data-pane="packages" hidden>
      <div class="card">
        <h2>Installed packages &mdash; ${(inv.packages || []).length}</h2>
        <div class="step">
          <input id="pkg-filter" placeholder="filter&hellip;" spellcheck="false"
            oninput="filterRows('pkg-filter','pkg-rows')"
            style="width:100%;padding:7px 10px;border:1px solid var(--line);border-radius:6px;background:var(--bg);color:var(--ink);font-family:var(--mono);font-size:12.5px">
        </div>
        <div class="scroll scrolly">
          <table><tbody id="pkg-rows">${(inv.packages || []).map((p) =>
            `<tr data-n="${esc((p.name || "").toLowerCase())}"><td>${esc(p.name)}</td>
             <td class="mono">${esc(p.version)}</td><td class="mono msg">${esc(p.source)}</td></tr>`).join("")}</tbody></table>
        </div>
      </div>

      ${cleanupCard(inv.cleanup, d.id, d.connected)}
    </div>

    <div data-pane="history" hidden>
      <div class="card">
        <h2>History &mdash; ${HISTORY_ALL ? "everything this machine has been asked to do" : "changes made to this machine"}</h2>
        <div class="step">
          <label class="msg" style="cursor:pointer">
            <input type="checkbox" ${HISTORY_ALL ? "checked" : ""} onchange="setHistoryAll(this.checked)">
            include scans and other read-only activity${hidden && !HISTORY_ALL ? ` (${hidden} hidden)` : ""}
          </label>
        </div>
        ${history.length ? history.map((c) => {
          const live = c.ok === null || c.ok === undefined;
          const st = live
            ? '<span class="pill">running</span>'
            : (c.ok ? '<span class="pill ok">ok</span>' : '<span class="pill bad">failed</span>');
          const out = (c.detail || c.progress || "").trim();
          return `<div style="padding:10px 14px;border-bottom:1px solid var(--line)">
            <div>${st} <b>${esc(c.kind)}</b>
              <span class="msg">&middot; ${new Date(c.created_at).toLocaleString()} &middot; ${ago(c.created_at)}</span><span class="mono msg cmdid" title="${esc(c.id)} - click to copy" onclick="copyText('${esc(c.id)}', this)">${esc(c.id.slice(0, 8))}</span></div>
            ${c.summary ? `<div class="mono" style="margin-top:4px">${esc(c.summary)}</div>` : ""}
            ${out ? `<details data-k="${esc(c.id)}" ontoggle="tailLog(this)"><summary>output</summary>
            <pre data-k="${esc(c.id)}" data-live="${live ? 1 : 0}">${esc(out)}</pre></details>` : ""}
          </div>`;
        }).join("") : `<div class="empty">Nothing has changed this machine yet.</div>`}
      </div>
    </div>`;

  // Only touches the DOM when something actually changed, so an open output
   // block, the package filter's text, and its caret all survive the timer.
  if (!setHTML(body, html)) return;
  applyPane();
}

// The credentials for a device being added. Held here rather than in the
// form, so a secret is never in the DOM where a screenshot or a stray copy
// would catch it.
let DEV_KEY = { key: "", secret: "", from: "" };

function deviceKindChanged() {
  const kind = $("dev-kind").value;
  // Only OPNsense uses a separate secret; the others carry a single token.
  $("dev-secret").hidden = kind !== "opnsense";
  $("dev-keyfile-label").textContent = kind === "opnsense"
    ? "API key file (the .txt OPNsense downloads)"
    : "key file (a text file holding the token)";
  $("dev-hint").textContent = {
    opnsense: "OPNsense: System > Access > Users > your user > API keys. Give that user only the `System: Firmware` privilege.",
    unraid: "Unraid: Settings > Management Access > API Keys. The VIEWER role with the INFO and OS resources is enough - do not use ADMIN.",
    homeassistant: "Home Assistant: your profile > Security > Long-lived access tokens > Create token. Paste the whole token. Include the port in the address, usually :8123.",
  }[kind] || "";
}

// Derive an id from the address, since it is nearly always the right one.
//
// The full name, not the short one: two sites each have a machine called
// `router`, and collapsing them to the first label makes the second one
// impossible to add.
function deviceUrlChanged() {
  const id = $("dev-id");
  if (id.dataset.touched === "1") return;
  const host = $("dev-url").value.trim()
    .replace(/^https?:\/\//, "").replace(/\/.*$/, "").replace(/:\d+$/, "");
  id.value = host;
  // The short name reads better as a label than the full one does.
  const name = $("dev-name");
  if (name && !name.value) name.placeholder = host.split(".")[0] || "name";
}

// The file an appliance hands you is `key=...` / `secret=...`, or just a bare
// key. Reading it in the browser means the secret goes straight into the
// manifest without anyone having to retype it.
function readKeyFile(input) {
  const file = input.files && input.files[0];
  if (!file) return;
  const reader = new FileReader();
  reader.onload = () => {
    const text = String(reader.result || "");
    const find = (name) => {
      const m = new RegExp("^\\s*" + name + "\\s*=\\s*(\\S+)", "mi").exec(text);
      return m ? m[1] : "";
    };
    const key = find("key") || text.trim().split(/\s+/)[0] || "";
    DEV_KEY = { key, secret: find("secret"), from: file.name };
    const st = $("dev-key-status");
    if (!key) {
      st.innerHTML = '<span class="bad">no key found in that file</span>';
      return;
    }
    st.innerHTML = `<span class="ok">read ${esc(DEV_KEY.secret ? "key and secret" : "key")} from ${esc(file.name)}</span>`;
    $("dev-key").value = "";
    $("dev-secret").value = "";
  };
  reader.readAsText(file);
}

function pastedKey() {
  DEV_KEY = {
    key: $("dev-key").value.trim(),
    secret: $("dev-secret").value.trim(),
    from: "pasted",
  };
  $("dev-key-status").innerHTML = "";
}

async function addDevice() {
  const st = $("dev-add-status");
  const say = (t, c) => { st.innerHTML = `<span class="${c}">${esc(t)}</span>`; };

  const kind = $("dev-kind").value;
  const raw = $("dev-url").value.trim();
  const id = $("dev-id").value.trim();
  if (!raw) return say("give it an address", "bad");
  if (!id) return say("give it an id", "bad");
  if (!DEV_KEY.key) return say("upload or paste an API key", "bad");
  if (kind === "opnsense" && !DEV_KEY.secret) {
    return say("OPNsense needs both a key and a secret", "bad");
  }

  // A bare hostname is what the probe wants; a pasted browser URL is what
  // people have to hand.
  const target = raw.replace(/^https?:\/\//, "").replace(/\/.*$/, "");

  // Home Assistant has no bespoke probe: it answers a plain HTTP request, and
  // the generic one already reads a version out of JSON. What it needs is
  // somewhere to learn the current release, which every device can now carry.
  let latest = {};
  let probe;
  if (kind === "opnsense") {
    probe = { type: "opnsense", api_key: DEV_KEY.key, api_secret: DEV_KEY.secret,
              insecure: true, check_after_hours: 12 };
  } else if (kind === "unraid") {
    probe = { type: "unraid", api_key: DEV_KEY.key, insecure: true, check_releases: true };
  } else {
    // Home Assistant tracks updates for itself, its add-ons and every device
    // it manages, so it answers the question directly - no release feed
    // needed.
    probe = {
      // The wire name is snake_case, like every other variant.
      type: "home_assistant",
      token: DEV_KEY.key,
      insecure: true,
      auto_update: "off",
    };
  }

  say("saving\u2026", "run");
  try {
    const m = await api("/api/manifest");
    m.devices = m.devices || [];
    const clash = m.devices.find((d) => d.id === id);
    if (clash) {
      return say(`there is already a device called ${id} (${clash.target || "no target"}). ` +
        `Use its full name to tell them apart.`, "bad");
    }
    m.devices.push({
      ...latest,
      id,
      label: $("dev-name").value.trim(),
      target,
      // The address that was pasted in is the management page, near enough:
      // it is where the operator just came from.
      url: raw.startsWith("http") ? raw : `https://${target}`,
      site: "",
      collector: "",
      probe,
    });
    await api("/api/manifest", { method: "PUT", body: JSON.stringify(m) });

    say("added \u2014 probing it now", "run");
    // Probe straight away rather than leaving a blank row until the next
    // sweep; whoever just added it wants to know it works.
    const portal = AGENTS.find((a) => a.hostname === location.hostname.split(".")[0])
      || AGENTS.find((a) => a.connected);
    if (portal) await cmd(portal.id, "probe_devices", { only: [id] });

    DEV_KEY = { key: "", secret: "", from: "" };
    $("dev-url").value = ""; $("dev-id").value = ""; $("dev-name").value = "";
    $("dev-id").dataset.touched = "";
    $("dev-key").value = ""; $("dev-secret").value = "";
    $("dev-keyfile").value = ""; $("dev-key-status").innerHTML = "";
    setTimeout(() => { say("added", "ok"); refresh(); }, 2500);
  } catch (e) {
    say(e.message, "bad");
  }
}

// What this device has looked like over time. Only readings that changed are
// kept, so the list is the story rather than a log of "still fine".
async function deviceHistory(id) {
  const box = $("device-history");
  box.hidden = false;
  box.innerHTML = `<div class="step"><div class="msg">loading&hellip;</div></div>`;
  let rows = [];
  try {
    rows = await api(`/api/devices/${encodeURIComponent(id)}/history`);
  } catch (e) {
    box.innerHTML = `<div class="step"><div class="msg bad">${esc(e.message)}</div></div>`;
    return;
  }

  box.innerHTML = `
    <h2>${esc(id)} &mdash; what changed</h2>
    <div class="step">
      <div class="msg">Every probe is compared with the one before it; a row appears only
        when something actually moved. ${rows.length ? "" : "Nothing recorded yet."}</div>
    </div>
    ${rows.length ? `<div class="scroll scrolly"><table>
      <thead><tr><th>When</th><th>Reachable</th><th>Version</th><th>Updates</th><th>By</th><th>Note</th></tr></thead>
      <tbody>${rows.map((r) => `<tr>
        <td>${new Date(r.checked_at).toLocaleString()} <span class="msg">${ago(r.checked_at)}</span></td>
        <td>${r.reachable ? '<span class="pill ok">yes</span>' : '<span class="pill bad">no</span>'}</td>
        <td class="mono">${esc(r.firmware || "-")}</td>
        <td>${r.updates === null || r.updates === undefined
          ? '<span class="msg">unknown</span>' : r.updates}</td>
        <td class="msg">${esc(r.collector)}</td>
        <td class="msg">${esc(r.error || (r.detail || "").split("\n")[0] || "")}</td>
      </tr>`).join("")}</tbody>
    </table></div>` : ""}
    <div class="bar"><button class="act" onclick="$('device-history').hidden = true">Close</button></div>`;
}

// Rename a device.
//
// Only the label: the id is what probe history is recorded against, and
// changing it would orphan everything remembered about the device.
// Where to go to act on this device. Same shape as renaming: edited in place,
// saved to the manifest, never touching the device itself.
// Let PatchPanel install updates on a device that supports it.
//
// Everything else here only ever looks. This is the exception, so turning it
// on is a deliberate answer to a question that spells out what changes.
async function setAutoUpdate(id, value) {
  if (value !== "off") {
    const what = value === "everything"
      ? "Home Assistant, its add-ons, AND the firmware of every device it manages"
      : "Home Assistant, its operating system, supervisor and add-ons";
    if (!confirm(
      `Let PatchPanel install updates on this device?\n\nIt will install: ${what}.\n\n` +
      "This happens on each probe, without asking again. Home Assistant takes its own " +
      "backup before updating itself." +
      (value === "everything"
        ? "\n\nDevice firmware cannot be rolled back. A failed write can leave hardware unusable."
        : "")
    )) {
      refresh();
      return;
    }
  }
  try {
    const m = await api("/api/manifest");
    const dev = (m.devices || []).find((d) => d.id === id);
    if (!dev || !dev.probe) return;
    dev.probe.auto_update = value;
    await api("/api/manifest", { method: "PUT", body: JSON.stringify(m) });
    refresh();
  } catch (e) {
    alert(e.message);
  }
}

async function setDeviceUrl(id, url) {
  const clean = (url || "").trim();
  try {
    const m = await api("/api/manifest");
    const dev = (m.devices || []).find((d) => d.id === id);
    if (!dev || (dev.url || "") === clean) return;
    dev.url = clean;
    await api("/api/manifest", { method: "PUT", body: JSON.stringify(m) });
    refresh();
  } catch (e) {
    alert(e.message);
  }
}

async function renameDevice(id, label) {
  const clean = (label || "").trim();
  try {
    const m = await api("/api/manifest");
    const dev = (m.devices || []).find((d) => d.id === id);
    if (!dev) return;
    if ((dev.label || "") === clean) return;
    dev.label = clean;
    await api("/api/manifest", { method: "PUT", body: JSON.stringify(m) });
    refresh();
  } catch (e) {
    alert(e.message);
  }
}

async function removeDevice(id) {
  if (!confirm("Stop monitoring " + id + "? Its API key is removed from the manifest too.")) return;
  const m = await api("/api/manifest");
  m.devices = (m.devices || []).filter((d) => d.id !== id);
  await api("/api/manifest", { method: "PUT", body: JSON.stringify(m) });
  refresh();
}

async function probeDevice(collectorId, id) {
  await cmd(collectorId, "probe_devices", { only: [id] });
}

function poolWhenChanged() {
  const kind = $("pool-when").value;
  $("pool-dow").hidden = kind !== "weekly";
  $("pool-time").hidden = kind === "manual";
}

function readSchedule() {
  const kind = $("pool-when").value;
  if (kind === "manual") return { kind: "manual" };
  const [h, m] = ($("pool-time").value || "03:00").split(":").map(Number);
  return kind === "daily"
    ? { kind: "daily", hour: h, minute: m }
    : { kind: "weekly", dow: Number($("pool-dow").value), hour: h, minute: m };
}

function scheduleText(sch) {
  if (!sch || sch.kind === "manual") return "manual";
  const days = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];
  const at = `${String(sch.hour).padStart(2, "0")}:${String(sch.minute).padStart(2, "0")}`;
  return sch.kind === "daily" ? `daily at ${at}` : `${days[sch.dow] || "?"} at ${at}`;
}

async function savePool() {
  const st = $("pool-status");
  const say = (t, c) => { st.innerHTML = `<span class="${c}">${esc(t)}</span>`; };
  const name = $("pool-name").value.trim();
  if (!name) return say("give it a name", "bad");
  try {
    await api("/api/pools", { method: "POST", body: JSON.stringify({
      name,
      scope: $("pool-scope").value,
      reboot: $("pool-reboot").value,
      schedule: readSchedule(),
      concurrency: Number($("pool-conc").value) || 1,
      exclude: [],
    })});
    $("pool-name").value = "";
    say("created", "ok");
    refresh();
  } catch (e) {
    say(e.message, "bad");
  }
}

async function updatePool(name, field, value) {
  const pools = await api("/api/pools");
  const p = pools.find((x) => x.name === name);
  if (!p) return;
  const body = {
    name, scope: p.scope, reboot: p.reboot, schedule: p.schedule,
    concurrency: p.concurrency, exclude: p.exclude || [],
  };
  body[field] = value;
  await api("/api/pools", { method: "POST", body: JSON.stringify(body) });
  refresh();
}

async function deletePool(name) {
  if (!confirm(`Delete the pool "${name}"?\n\nIts machines are not touched - they simply stop belonging to a pool.`)) return;
  await api(`/api/pools/${encodeURIComponent(name)}`, { method: "DELETE" });
  refresh();
}

async function runPool(name) {
  if (!confirm(`Run "${name}" now?\n\nThis patches its machines according to the pool's policy, without waiting for the schedule.`)) return;
  try {
    const r = await api(`/api/pools/${encodeURIComponent(name)}/run`, { method: "POST" });
    alert(`Dispatched to ${r.dispatched} machine(s). The rest follow as slots free up.`);
    refresh();
  } catch (e) {
    alert(e.message);
  }
}

async function setMachinePool(id, pool) {
  try {
    await api(`/api/agents/${id}/pool`, { method: "POST", body: JSON.stringify({ pool }) });
    refresh();
  } catch (e) {
    alert(e.message);
  }
}

async function loadBackups() {
  const d = await api("/api/backups");
  const s = d.summary;

  $("backup-empty").hidden = d.guests.length > 0;
  setHTML($("backup-tiles"),
    tile(s.guests, "guests") +
    tile(s.fresh, "backed up") +
    tile(s.unprotected_running, "at risk", "bad") +
    tile(s.stale, "stale") +
    tile(s.never, "never") +
    (s.exempt ? tile(s.exempt, "not tracked") : "") +
    tile(s.jobs_running, "running") +
    tile(s.jobs_stuck, "stuck", "bad") +
    tile(s.jobs_failed, "recent failures", "bad"));

  // The number that matters is running guests nobody is protecting.
  const alerts = [];
  if (s.unprotected_running) {
    alerts.push(`<div class="card" style="border-color:var(--bad)"><div class="step">
      <div class="note" style="border-color:var(--bad)">
        <b>${s.unprotected_running} running guest(s) have no recent backup.</b>
        They are doing work right now and there is nothing to restore them from. Machines that
        are switched off are listed below too, but those are usually templates and matter less.
      </div>
    </div></div>`);
  }
  if (s.jobs_stuck) {
    alerts.push(`<div class="card" style="border-color:var(--bad)"><div class="step">
      <div class="note" style="border-color:var(--bad)">
        <b>${s.jobs_stuck} backup job(s) have been running for over 12 hours.</b>
        That is usually stuck rather than slow &mdash; worth looking at on the host itself.
      </div>
    </div></div>`);
  }
  setHTML($("backup-alerts"), alerts.join(""));

  setHTML($("backup-guests"), d.guests.map((g) => {
    const on = (g.state || "").toLowerCase().startsWith("running");
    // A stopped guest with no backup is usually a template nobody intends to
    // keep. Saying it in the same red as a running machine with nothing to
    // restore from makes the red mean less.
    // A non-default expectation is shown beside the age, not folded into it:
    // "18 days" is fine for a guest wanted quarterly and a problem for one
    // wanted weekly, and the row has to say which.
    const cadence = g.cadence && g.status !== "exempt"
      ? ` <span class="msg" title="You asked for a backup about every ${g.every_days} days.">${esc(g.cadence)}</span>`
      : "";
    const status = g.status === "snoozed"
      ? `<span class="pill" title="You asked to be reminded on ${
          new Date(g.snooze_until).toLocaleDateString()}. It is still a gap - just not one on the list until then.">
          ${g.last_backup ? ago(g.last_backup) : "never"} &middot; back ${ago(g.snooze_until)}</span>`
      : g.status === "exempt"
      ? `<span class="pill" title="${esc(g.reason || "Deliberately not tracked.")}">not tracked</span>${
          g.last_backup ? ` <span class="msg">last ${ago(g.last_backup)}</span>` : ""}`
      : g.status === "fresh"
        ? `<span class="pill ok">${ago(g.last_backup)}</span>${cadence}`
        : g.status === "stale"
          ? `<span class="pill ${on ? "warn" : ""}" title="Wanted about every ${g.every_days} days.">${ago(g.last_backup)}</span>${cadence}`
          : `<span class="pill ${on ? "bad" : ""}" title="${on
              ? "This guest is running and has nothing to restore from."
              : "Never backed up, but it is not running - often a template."}">never</span>${cadence}`;
    const quiet = g.status === "exempt" || g.status === "snoozed";
    return `<tr${on && !quiet ? "" : ' style="opacity:.55"'}>
      <td>${esc(g.name)}<div class="mono msg">${esc(g.id)}</div></td>
      <td>${esc(g.host)}</td>
      <td class="mono msg">${esc(g.kind)}</td>
      <td>${esc(g.state) || "-"}</td>
      <td>${g.managed ? '<span class="pill ok">yes</span>' : '<span class="msg">no</span>'}</td>
      <td>${status}</td>
      <td><select class="fld" style="font-size:11.5px;padding:3px 6px"
          title="How often you want this one backed up. It stays counted either way - this only decides when a gap is a gap."
          onchange="setBackupCadence(${jsq(g.host)}, ${jsq(g.id)}, ${jsq(g.name)}, this.value)">
          ${[["", "on the usual schedule"], ["30", "about monthly"], ["90", "about quarterly"],
             ["365", "about yearly"], ["0", "don't track it"]].map(([v, label]) => {
            const current = g.status === "exempt" ? "0"
              : !g.cadence ? ""
              : String(g.every_days);
            return `<option value="${v}"${v === current ? " selected" : ""}>${label}</option>`;
          }).join("")}
        </select>
        ${g.status === "stale" || g.status === "never"
          ? `<button class="act" style="padding:2px 8px;font-size:11.5px;margin-left:4px"
               title="Take it off the list for a month. It comes back on its own, and the row says when."
               onclick="snoozeBackup(${jsq(g.host)}, ${jsq(g.id)})">Remind me in a month</button>`
          : g.status === "snoozed"
            ? `<button class="act" style="padding:2px 8px;font-size:11.5px;margin-left:4px"
                 title="Put it back on the list now."
                 onclick="unsnoozeBackup(${jsq(g.host)}, ${jsq(g.id)})">Remind me now</button>`
            : ""}
        ${g.reason ? `<div class="msg">${esc(g.reason)}</div>` : ""}
        <div class="msg">${g.last_backup ? new Date(g.last_backup).toLocaleString() : ""}</div></td>
    </tr>`;
  }).join(""));

  setHTML($("backup-hosts"), d.hosts.map((h) => `<div class="card">
    <h2>${esc(h.host)} <span class="msg">${esc(h.platform)}</span></h2>
    ${backupState(h)}
  </div>`).join(""));

  const bad = s.unprotected_running;
  const el = $("badge-backups");
  if (el) {
    el.hidden = false;
    el.textContent = bad > 99 ? "99+" : String(bad);
    el.className = "badge" + (bad === 0 ? " ok" : " bad");
    el.title = bad
      ? `${bad} running guest(s) with no recent backup`
      : `${s.fresh} backed up; nothing running is unprotected`;
  }
}

// Mark a guest as one nobody intends to back up.
//
// It stays on the page, greyed, with the reason: a decision that disappears is
// one nobody can revisit, and "why is that not backed up" deserves an answer
// six months later.
// Only the strongest choice asks why. Saying "this one is fine quarterly" is
// self-explanatory on the row afterwards; saying "never look at this again"
// is not, and "why is that not backed up" is a question asked six months
// later, when the reason is the only part anyone still needs.
async function snoozeBackup(host, guest) {
  try {
    await api("/api/backups/snooze", {
      method: "POST",
      body: JSON.stringify({ host, guest, snooze_days: 30 }),
    });
    refresh();
  } catch (e) {
    alert(e.message);
  }
}

// Ending a snooze early is the same call as returning to the fleet default,
// because a snooze is the only thing a bare rule with no cadence holds.
async function unsnoozeBackup(host, guest) {
  try {
    await api("/api/backups/exempt", {
      method: "DELETE",
      body: JSON.stringify({ host, guest }),
    });
    refresh();
  } catch (e) {
    alert(e.message);
  }
}

async function setBackupCadence(host, guest, name, value) {
  try {
    if (value === "") {
      await api("/api/backups/exempt", {
        method: "DELETE",
        body: JSON.stringify({ host, guest }),
      });
    } else if (value === "0") {
      const reason = prompt(
        `Stop tracking backups for ${name}?\n\n` +
        "It stays in the list with your reason, and stops being counted as a gap.\n\n" +
        "Why? (optional, but future-you will want it)"
      );
      if (reason === null) { refresh(); return; }
      await api("/api/backups/exempt", {
        method: "POST",
        body: JSON.stringify({ host, guest, reason: reason.trim(), every_days: 0 }),
      });
    } else {
      await api("/api/backups/exempt", {
        method: "POST",
        body: JSON.stringify({ host, guest, reason: "", every_days: Number(value) }),
      });
    }
    refresh();
  } catch (e) {
    alert(e.message);
  }
}

// Every time a schedule fires between two dates.
//
// The portal only reports the next occurrence; a calendar needs all of them,
// and the rule is simple enough to walk day by day rather than inventing a
// second implementation of it on the server.
function occurrences(schedule, from, days) {
  if (!schedule || schedule.kind === "manual") return [];
  const out = [];
  for (let i = 0; i < days; i++) {
    const d = new Date(from);
    d.setDate(d.getDate() + i);
    if (schedule.kind === "weekly") {
      // dow 0 is Monday, as people say it; getDay() has Sunday at 0.
      const mondayFirst = (d.getDay() + 6) % 7;
      if (mondayFirst !== schedule.dow) continue;
    }
    d.setHours(schedule.hour, schedule.minute, 0, 0);
    out.push(new Date(d));
  }
  return out;
}

const MONTHS = ["Jan","Feb","Mar","Apr","May","Jun","Jul","Aug","Sep","Oct","Nov","Dec"];
const hhmm = (d) => String(d.getHours()).padStart(2, "0") + ":" +
  String(d.getMinutes()).padStart(2, "0");

// Three weeks back, the current week, two forward. The grid starts on a Monday
// so a weekly schedule lands in one column and its rhythm is visible as a
// shape rather than as a list of dates.
function renderCalendar(pools, past) {
  const BACK = 3, FORWARD = 2;
  const today = new Date();
  today.setHours(0, 0, 0, 0);
  const start = new Date(today);
  start.setDate(start.getDate() - ((start.getDay() + 6) % 7) - BACK * 7);
  const DAYS = (BACK + 1 + FORWARD) * 7;

  const byDay = new Map();
  const push = (at, html) => {
    const k = at.toDateString();
    if (!byDay.has(k)) byDay.set(k, []);
    byDay.get(k).push({ at, html });
  };

  // What happened, as the portal reconstructed it from the command log.
  for (const e of past || []) {
    const at = new Date(e.at);
    if (at < start) continue;
    push(at, `<div class="ev ${esc(e.outcome)}${
      e.source === "manual" ? " manual" : e.source === "job" ? " job" : ""}"
      title="${esc(e.label)} &mdash; ${esc(e.detail)}${
        e.source === "manual" ? " (you started this)"
        : e.source === "unknown" ? " (ran before the portal recorded who started a command)"
        : ""}">
      <span class="t">${hhmm(at)}</span> ${esc(e.label)}<span class="o">${esc(e.detail)}</span></div>`);
  }

  // What will happen. Only the first occurrence of each pool gets words; the
  // rest are ticks, because the fifteenth identical cell says nothing the
  // first one did not.
  const now = new Date();
  for (const p of pools) {
    if (p.scope === "none") continue;
    let spelled = false;
    for (const at of occurrences(p.schedule, start, DAYS)) {
      if (at <= now) continue;
      if (!spelled) {
        spelled = true;
        const due = (p.plan || []).filter((x) => x.action === "patch").length;
        push(at, `<div class="ev next"
          title="${esc(p.name)} &mdash; ${p.members.length} machine(s), installs ${esc(p.scope)}${
            p.reboot === "if-needed" ? ", may reboot" : ", no reboot"}">
          <span class="t">${hhmm(at)}</span> ${esc(p.name)}<span class="o">${
            due ? `${due} would patch` : "none due"}</span></div>`);
      } else {
        push(at, `<div class="tick" title="${esc(p.name)} &mdash; ${hhmm(at)}"></div>`);
      }
    }
  }

  const cells = [];
  for (let i = 0; i < DAYS; i++) {
    const day = new Date(start);
    day.setDate(day.getDate() + i);
    const evs = (byDay.get(day.toDateString()) || []).sort((a, b) => a.at - b.at);
    const isToday = day.toDateString() === today.toDateString();
    // A day with more than three things on it gets three and a count; the
    // alternative is one cell as tall as the week it sits in.
    const shown = evs.slice(0, 3).map((e) => e.html).join("");
    const rest = evs.length > 3 ? `<div class="msg">+${evs.length - 3} more</div>` : "";
    cells.push(`<div class="day${isToday ? " today" : ""}${
      day.getMonth() !== today.getMonth() && !isToday ? " out" : ""}">
      <div class="d"><b>${day.getDate()}</b>${
        day.getDate() === 1 || i === 0 ? `<span>${MONTHS[day.getMonth()]}</span>` : ""}</div>
      ${shown}${rest}</div>`);
  }
  setHTML($("pool-calendar"), cells.join(""));
}

// A job is only as good as the expectation attached to it: without
// `every_hours` the portal will say it last ran and nothing more, because
// inventing a schedule for someone else's script would produce a warning
// nobody asked for and cannot answer.
// Jobs group by the part of the name before the first slash, so everything
// one watcher reports sits together under a heading. A name with no slash is
// a job reporting for itself, which is its own group.
function jobGroups(jobs) {
  const groups = new Map();
  for (const j of jobs) {
    const cut = j.name.indexOf("/");
    const key = cut > 0 ? j.name.slice(0, cut) : "";
    if (!groups.has(key)) groups.set(key, []);
    groups.get(key).push(j);
  }
  // Named groups first and alphabetical; the ungrouped ones last, because a
  // heading is a promise that what follows belongs together.
  return [...groups.entries()].sort((a, b) =>
    (a[0] ? 0 : 1) - (b[0] ? 0 : 1) || a[0].localeCompare(b[0]));
}

function splitMuted(jobs) {
  return [jobs.filter((j) => !j.muted), jobs.filter((j) => j.muted)];
}

// One job, drawn the same whether it is live or ignored. `key` is the group
// prefix, stripped from the displayed name because the heading already says it.
function jobRow(j, key) {
    const pill = j.muted
      ? '<span class="pill" title="You asked not to be told about this one. It still runs and is still recorded.">ignored</span>'
      : j.status === "suspect"
        ? `<span class="pill warn" title="It reported success, but its own output reported problems. Open History to see them.">output disagrees</span>`
      : j.status === "overdue"
      ? `<span class="pill bad" title="Expected every ${j.every_hours}h; nothing has arrived.">stopped checking in</span>`
      : j.status === "failed"
        ? '<span class="pill bad">last run failed</span>'
        : j.status === "quiet"
          ? '<span class="pill">never reported</span>'
          : j.every_hours
            ? '<span class="pill ok">on time</span>'
            : '<span class="pill" title="It has not said how often it runs, so it cannot be late.">no schedule given</span>';

    const facts = Object.entries(j.facts || {}).map(([k, v]) => {
      // When a reported value last moved is usually the interesting part -
      // a public IP that changed this morning explains a lot of other things.
      const moved = (j.history || []).find((h) => h.key === k);
      return `<span class="msg">${esc(k.replace(/_/g, " "))}
        <b>${esc(v)}</b>${moved ? ` <span title="${new Date(moved.at).toLocaleString()}">changed ${ago(moved.at)}</span>` : ""}</span>`;
    }).join("");

    return `<div class="job">
      <div>
        <div class="nm">${esc(key ? j.name.slice(key.length + 1) : j.name)} ${pill}</div>
        ${j.last_detail ? `<div class="msg">${esc(j.last_detail)}</div>` : ""}
        ${facts ? `<div class="facts">${facts}</div>` : ""}
      </div>
      <div class="when">
        <div class="msg">${j.last_at ? `ran ${ago(j.last_at)}` : "never"}</div>
        ${j.due_at ? `<div class="msg">due ${ago(j.due_at)}</div>` : ""}
        <button class="act" style="padding:2px 8px;font-size:11.5px;margin-top:4px"
          title="Every run this job has reported, with what it printed."
          onclick="jobHistory(${jsq(j.name)})">History</button>
        <button class="act" style="padding:2px 8px;font-size:11.5px;margin-top:4px"
          title="${j.muted
            ? "Start counting this job again."
            : "Stop being told about this one. It keeps running and keeps its history; it just stops asking for attention."}"
          onclick="muteJob(${jsq(j.name)}, ${j.muted ? "false" : "true"})">${
            j.muted ? "Count it again" : "Ignore"}</button>
        <button class="act" style="padding:2px 8px;font-size:11.5px;margin-top:4px"
          title="Remove this job. It comes back if it checks in again."
          onclick="forgetJob(${jsq(j.name)})">Forget</button>
      </div>
    </div>`;
}

// Keys that are counters rather than readings. They belong in the job row's
// summary line, not on a board someone scans for an address or a filename.
const TALLY_KEYS = new Set(["ok", "failed", "changed", "skipped", "errors",
  "warnings", "success", "succeeded", "scripts_checked", "unfinished"]);

// When each value last actually moved.
//
// The portal only records a fact when it changes, so the history is already a
// list of moves rather than of check-ins - which is what makes "since" a real
// answer rather than "whenever this last ran".
function lastChanged(job, key) {
  return (job.history || []).find((h) => h.key === key);
}

// A board of what the jobs know: the address at each site and when it arrived,
// the last backup and what it is called. Built from whatever the jobs report,
// so a new value appears here on its own.
function renderJobStatus(jobs) {
  const now = Date.now();
  const panels = jobs
    .filter((j) => !j.muted && Object.keys(j.facts || {}).length)
    .map((j) => {
      const readings = Object.entries(j.facts)
        .filter(([k]) => !TALLY_KEYS.has(k))
        .sort(([a], [b]) => a.localeCompare(b));
      if (!readings.length) return "";

      const rows = readings.map(([k, v]) => {
        const moved = lastChanged(j, k);
        const fresh = moved && now - new Date(moved.at).getTime() < 864e5;
        return `<div class="reading${fresh ? " fresh" : ""}">
          <span class="k">${esc(k.replace(/_/g, " "))}</span>
          <span class="v" title="${esc(v)}">${esc(v)}</span>
          <span class="since" title="${moved
            ? "Last changed " + new Date(moved.at).toLocaleString()
            : "Unchanged for as long as this has been recorded"}">${
            moved ? ago(moved.at) : "&mdash;"}</span>
        </div>`;
      }).join("");

      // The counters stay, but as one quiet line under the heading rather
      // than as readings competing with the values people came for.
      const tallies = Object.entries(j.facts)
        .filter(([k]) => TALLY_KEYS.has(k))
        .map(([k, v]) => `${esc(v)} ${esc(k.replace(/_/g, " "))}`)
        .join(" &middot; ");

      const name = j.name.includes("/") ? j.name.slice(j.name.indexOf("/") + 1) : j.name;
      return `<div class="panel">
        <h3>${esc(name)}
          ${j.status === "failed" || j.status === "overdue"
            ? '<span class="pill bad">needs you</span>'
            : j.status === "suspect" ? '<span class="pill warn">output disagrees</span>' : ""}
          <span class="when">${j.last_at ? "ran " + ago(j.last_at) : "never run"}</span></h3>
        ${tallies ? `<div class="reading"><span class="k">this run</span>
          <span class="v" style="font-family:inherit;color:var(--muted)">${tallies}</span>
          <span class="since"></span></div>` : ""}
        ${rows}
      </div>`;
    })
    .filter(Boolean);

  setHTML($("job-status"), panels.length ? `<div class="board">${panels.join("")}</div>` : "");
}

function renderJobs(jobs) {
  renderJobStatus(jobs);
  $("jobs-empty").hidden = jobs.length > 0;
  const [live, muted] = splitMuted(jobs);
  const groups = jobGroups(live);
  setHTML($("job-rows"), groups.map(([key, list]) => {
    const bad = list.filter((j) => j.status === "overdue" || j.status === "failed").length;
    const head = key
      ? `<div class="jobgroup"><b>${esc(key)}</b>
           <span class="msg">${list.length} job(s)${bad ? ` &middot; ${bad} need you` : ""}</span></div>`
      : (groups.length > 1
          ? `<div class="jobgroup"><b>Other</b>
               <span class="msg">${list.length} job(s)</span></div>`
          : "");
    return head + list.map((j) => jobRow(j, key)).join("");
  }).join("") + (muted.length
    ? `<div class="jobgroup"><b>Ignored</b>
         <span class="msg">${muted.length} job(s) &middot; still running, still recorded,
         not counted anywhere</span></div>` + muted.map((j) => jobRow(j, "")).join("")
    : ""));
}

async function loadJobs() {
  try {
    renderJobs(await api("/api/jobs"));
  } catch (e) {
    /* an older portal has no jobs endpoint */
  }
}

function showJobSetup() {
  const box = $("job-setup");
  box.hidden = !box.hidden;
  if (box.hidden) return;
  // Written for Unraid's User Scripts, which is where this usually lives.
  //
  // POSIX sh on purpose: those scripts are `#!/bin/sh`, and `trap ... ERR` -
  // the obvious way to write this - is a bashism that sh accepts and silently
  // never fires. An EXIT trap plus a flag works in both, and covers the case
  // that matters most: the script exiting early, before the success line it
  // would otherwise have reached.
  box.innerHTML = `<div class="msg">Paste this at the top of the script. It keeps
    its own copy of the helper current, and &mdash; more importantly &mdash; cannot
    break the script if this portal is unreachable.</div>
    <pre>PP_URL=http://${esc(location.host)}
PP_LIB=/boot/config/pp-report.sh
if curl -fsS -m 5 "$PP_URL/pp-report.sh" -o "$PP_LIB.new" 2&gt;/dev/null &amp;&amp;
   grep -q "^pp_init()" "$PP_LIB.new"; then mv "$PP_LIB.new" "$PP_LIB"; fi
rm -f "$PP_LIB.new"
if [ -r "$PP_LIB" ]; then . "$PP_LIB"; else
  pp_init() { :; }; pp_fact() { :; }; pp_ok() { :; }
  pp_fail() { :; }; pp_finish() { :; }
fi

pp_init router-backup 24     # job name, and how many hours between runs

# ... the work, calling pp_fact as it learns things ...
pp_fact public_ip "$IP"

pp_finish "$FAIL" "$OK ok, $CHANGED changed"</pre>
    <div class="msg">Three fallbacks, in order: a fresh copy, the cached copy,
      then stub functions that do nothing. Monitoring that can take down the
      thing it monitors is worse than no monitoring, so an unreachable portal
      leaves the job running and simply unreported. The <code>grep</code> stops a
      login page or proxy error that arrived with a 200 from being cached and
      then sourced.</div>
    <div class="msg"><code>pp_init</code> goes <b>above</b> any early exit, including
      missing-credential checks &mdash; those are usually a script's quietest failure,
      because skipping the work also skips whatever it normally complains with.
      It installs an exit trap, so a script that dies part-way still reports one.
      The second argument is the load-bearing one: without it the portal can only
      say when the job last ran, and a run that never happens is the failure
      nothing else will ever mention.</div>
    <div class="msg">Also available: <code>pp_ok "detail"</code> and
      <code>pp_fail "detail"</code>. Set <code>PP_TOKEN</code> before sourcing if
      this portal requires a token.</div>`;
}

// Every run, newest first, with what it printed.
//
// The list answers "is this working"; this answers "what happened that night",
// which is the question you actually have at the point you open it.
async function jobHistory(name) {
  const box = $("job-history");
  const runs = await api("/api/jobs/" + encodeURIComponent(name) + "/runs");
  box.hidden = false;
  setHTML(box, `<h2>${esc(name)} <span class="sub">${runs.length} recorded run(s)</span>
      <button class="act" style="float:right;padding:2px 8px;font-size:11.5px"
        onclick="$('job-history').hidden = true">Close</button></h2>
    ${runs.length ? runs.map((r, i) => `<div class="step">
      <div>
        <span class="pill ${r.suspect ? "warn" : r.ok ? "ok" : "bad"}">${
          r.suspect ? "output disagrees" : r.ok ? "ok" : "failed"}</span>
        <span class="mono">${new Date(r.at).toLocaleString()}</span>
        <span class="msg">${ago(r.at)}</span>
      </div>
      ${r.detail ? `<div class="msg">${esc(r.detail)}</div>` : ""}
      ${r.log
        ? `<details${i === 0 ? " open" : ""}><summary class="msg">what it printed</summary>
             <pre>${esc(r.log)}</pre></details>`
        : `<div class="msg">No output kept for this run${
             i > 6 ? " - output older than 30 days is discarded" : ""}.</div>`}
    </div>`).join("") : '<div class="empty">Nothing recorded yet.</div>'}`);
  box.scrollIntoView({ behavior: "smooth", block: "nearest" });
}

// Not a delete. The job keeps running, keeps reporting and keeps its history;
// it just stops appearing on Overview and in the badge. Hiding something
// outright is how a dashboard starts lying, so it stays on the page, greyed,
// under a heading that says what was decided.
async function muteJob(name, muted) {
  try {
    await api("/api/jobs/" + encodeURIComponent(name) + "/mute", {
      method: "POST",
      body: JSON.stringify({ muted }),
    });
    refresh();
  } catch (e) {
    alert(e.message);
  }
}

async function forgetJob(name) {
  if (!confirm("Forget " + name + "? Its history goes too. It reappears if it checks in again.")) return;
  try {
    await api("/api/jobs/" + encodeURIComponent(name), { method: "DELETE" });
    refresh();
  } catch (e) {
    alert(e.message);
  }
}

async function loadPools() {
  const pools = await api("/api/pools");
  // The past is a separate question from the schedule, and a separate query.
  let past = [];
  try { past = await api("/api/schedule"); } catch (e) { past = []; }
  renderCalendar(pools, past);

  setHTML($("pool-list"), pools.map((p) => {
    const willPatch = p.plan.filter((x) => x.action === "patch");
    return `<div class="card">
      <h2>${esc(p.name)}
        ${p.scope === "none" ? '<span class="pill">patches nothing</span>'
          : `<span class="pill ${p.scope === "all" ? "warn" : ""}">${esc(p.scope)}</span>`}
        ${p.reboot === "if-needed" ? '<span class="pill warn">may reboot</span>' : ""}
      </h2>
      <div class="step">
        ${kv([
          ["Schedule", esc(scheduleText(p.schedule)) +
            (p.next_run ? ` <span class="msg">&middot; next ${new Date(p.next_run).toLocaleString()}</span>` : "")],
          ["Machines", `${p.members.length}`],
          ["At once", `${p.concurrency}`],
          ["Last run", p.last_run ? `${ago(p.last_run)}` : "never"],
        ])}
      </div>
      <div class="step">
        <h3>If it ran now</h3>
        ${p.plan.length ? `<div class="scroll" style="max-height:220px;overflow:auto"><table>
          <tbody>${p.plan.map((x) => `<tr>
            <td>${esc(x.hostname)}</td>
            <td>${x.action === "patch" ? '<span class="pill warn">patch</span>'
              : x.action === "wait" ? '<span class="pill">wait</span>'
              : x.action === "skip" ? '<span class="pill bad">skip</span>'
              : '<span class="pill ok">nothing</span>'}</td>
            <td class="msg">${esc(x.detail)}</td>
          </tr>`).join("")}</tbody></table></div>`
          : `<div class="msg">No machines in this pool yet.</div>`}
        <div class="msg" style="margin-top:8px">${willPatch.length
          ? `${willPatch.length} machine(s) would be patched, ${p.concurrency} at a time.`
          : "Nothing to do right now."}</div>
      </div>
      <div class="bar">
        <button class="act" ${p.scope === "none" ? "disabled" : ""}
          onclick="runPool(${jsq(p.name)})">Run now</button>
        <select class="fld" onchange="updatePool(${jsq(p.name)}, 'scope', this.value)">
          ${["none", "security", "all"].map((v) =>
            `<option value="${v}"${p.scope === v ? " selected" : ""}>${v === "none" ? "patch nothing" : v === "security" ? "security only" : "everything"}</option>`).join("")}
        </select>
        <select class="fld" onchange="updatePool(${jsq(p.name)}, 'reboot', this.value)">
          <option value="never"${p.reboot === "never" ? " selected" : ""}>never reboot</option>
          <option value="if-needed"${p.reboot === "if-needed" ? " selected" : ""}>reboot if needed</option>
        </select>
        <button class="act" onclick="deletePool(${jsq(p.name)})">Delete</button>
      </div>
    </div>`;
  }).join("") || `<div class="card"><div class="empty">No pools yet.</div></div>`);

  const inPool = {};
  pools.forEach((p) => p.members.forEach((m) => { inPool[m] = p.name; }));

  setHTML($("pool-members"), AGENTS.map((a) => `<tr>
    <td><a href="#agent/${a.id}" style="color:inherit">${esc(a.hostname)}</a></td>
    <td class="msg">${esc(a.os_version)}</td>
    <td>${a.actionable_count || 0}${a.security_count ? ` <span class="pill bad">${a.security_count} sec</span>` : ""}</td>
    <td>
      <select class="fld" onchange="setMachinePool(${jsq(a.id)}, this.value)">
        <option value=""${!inPool[a.id] ? " selected" : ""}>&mdash; none &mdash;</option>
        ${pools.map((p) => `<option value="${esc(p.name)}"${inPool[a.id] === p.name ? " selected" : ""}>${esc(p.name)}</option>`).join("")}
      </select>
    </td>
  </tr>`).join(""));
}

// Who is sending, and how much. The address is the identity: the hostname in
// a syslog message is written by the sender, and the address is not.
// Sweep again, from the card that shows what a sweep found.
//
// Read-only, so it needs no confirmation - but it takes a minute on a /24, and
// a button that looks like it did nothing gets pressed repeatedly.
// Which discovered host is expanded, if any.
//
// Held outside the render because the table redraws every few seconds; an open
// row has to survive that, and re-fetching on each redraw would make it flicker.
let OPEN_HOST = null;

function toggleHost(ip) {
  OPEN_HOST = OPEN_HOST === ip ? null : ip;
  // Redraw from what is already in hand rather than asking the portal again:
  // the detail is in the payload the table was built from, so opening a row
  // needs no round trip and cannot show something different from its own row.
  if (LAST_DEVICES) renderNetwork(LAST_DEVICES);
}

// Everything the sweep learned about one host, as a row beneath its own row.
//
// Inline rather than a panel at the foot of the page: the detail belongs where
// the click was, and a card lower down means scrolling away from the thing you
// were looking at and losing your place in a table of thirty.
function hostDetail(h, columns) {
  const id = h.identity || {};
  const svc = new Map((h.services || [])
    .map((x) => [`${x.port}/${x.protocol || "tcp"}`, x]));

  // Script output is raw text nmap printed, so it is shown as such rather than
  // parsed into fields this page would then have to keep in step with nmap.
  const scripts = (list) => (list || []).map(([script, text]) => `<div class="reading">
      <span class="k mono">${esc(script)}</span>
      <span class="v" style="white-space:normal">${esc(text)}</span>
    </div>`).join("");

  const rows = [
    ...(h.open_ports || []).map((p) => [p, "tcp"]),
    ...(h.open_udp || []).map((p) => [p, "udp"]),
  ];
  const ports = rows.map(([port, proto]) => {
    // Matched on both, because 161/udp and 161/tcp are separate entries and
    // keying on the number alone would show one port's findings under the
    // other's.
    const s = svc.get(`${port}/${proto}`);
    const named = s && [s.name, s.product, s.version].filter((x) => x).join(" ");
    return `<div class="reading">
      <span class="k mono">${port}${proto === "udp" ? "/udp" : ""}</span>
      <span class="v">${named ? esc(named) : "&mdash;"}${
        s && s.extra ? ` <span class="msg">${esc(s.extra)}</span>` : ""}</span>
      <span class="since">${s
        ? (s.product ? "identified" : "guessed from the port")
        : "open, nothing identified"}</span>
    </div>${scripts(s && s.scripts)}${s && (s.cpe || []).length ? s.cpe.map((c) => `<div class="reading">
      <span class="k"></span>
      <span class="v" style="color:var(--muted)">${esc(c)}</span>
      <span class="since">what to search a CVE list for</span>
    </div>`).join("") : ""}`;
  }).join("");

  // Every one of these is a guess except the vendor, so each says how it was
  // arrived at. An OS fingerprint shown as a fact is the kind of thing somebody
  // acts on and then spends an afternoon confused by.
  // Reverse DNS is not a guess and not a fact either - it is a record somebody
  // wrote and may never have revisited - so it is presented as exactly that.
  const facts = [
    (id.hostnames || []).length
      ? ["Reverse DNS", `${esc(id.hostnames.join(", "))}
          <span class="msg">a PTR record, so whatever was written down when it was
          set up &mdash; not checked against the host itself</span>`]
      : null,
    id.device_type
      ? ["Kind of device", `${dotted([id.device_type, id.vendor, id.os_family])}
          <span class="msg">nmap's classification of the fingerprint, same confidence
          as the OS guess below</span>`]
      : null,
    h.known
      ? ["Known as", `${esc(h.known.name)} <span class="msg">${h.known.role}, matched by ${
          esc(h.known.via || "address")}</span>${
          h.known.agent
            ? ` &middot; <a href="#" onclick="openAgent(${jsq(h.known.agent)});return false">open its machine page</a>`
            : ""}`]
      : ["Known as", `<span class="pill warn">nothing accounts for this address</span>
          <span class="msg">not a machine in this fleet and not a declared appliance</span>`],
    id.mac_vendor
      ? ["Vendor", `${esc(id.mac_vendor)} <span class="msg">from the hardware address, assigned not guessed</span>`]
      : null,
    id.mac ? ["Hardware address", `<span class="mono">${esc(id.mac)}</span>`] : null,
    id.os
      ? ["Operating system", `${esc(id.os)} <span class="msg">${id.os_accuracy}% confident${
          (id.os_alternatives || []).length
            ? ` &middot; also considered ${esc(id.os_alternatives.join(", "))}`
            : ""}</span>`]
      : null,
    id.uptime_secs
      ? ["Uptime", `${dur(id.uptime_secs)}
          <span class="msg">inferred from TCP timestamps, approximate</span>`]
      : null,
    (id.os_cpe || []).length
      ? ["Platform", `<span class="mono" style="color:var(--muted)">${esc(id.os_cpe.join(" "))}</span>
          <span class="msg">what to search a CVE list for</span>`]
      : null,
    ["Scanner", h.scanner === "nmap"
      ? 'nmap <span class="msg">service and OS detection on</span>'
      : 'built-in TCP sweep <span class="msg">banner only; no services, vendor or OS</span>'],
  ].filter(Boolean);


  return `<tr class="detail"><td colspan="${columns}">
    ${kv(facts)}
    <div class="step" style="padding-left:0">
      <div class="msg">Per port. A product name is evidence; a bare service name is
        nmap reading the port number, which is a guess and labelled as one.</div>
      ${ports || '<div class="msg">No open ports recorded.</div>'}
    </div>
    ${(id.scripts || []).length ? `<div class="step" style="padding-left:0">
      <div class="msg">What nmap's identifying scripts reported about the host.</div>
      ${scripts(id.scripts)}
    </div>` : ""}
    ${h.known ? "" : `<div class="msg">Not managed by PatchPanel. Add it as an appliance under
      <a href="#setup" onclick="showTab('setup');return false">Setup</a> with target
      <span class="mono">${esc(h.ip)}</span>, or install an agent on it.</div>`}
  </td></tr>`;
}

async function scanNow(btn) {
  const was = btn.textContent;
  btn.disabled = true;
  btn.textContent = "Sweeping…";
  try {
    const r = await api("/api/commands/broadcast", {
      method: "POST",
      body: JSON.stringify({ command: { kind: "discover" } }),
    });
    btn.textContent = `Sent to ${r.dispatched_to}`;
    // Only agents with a discovery range configured do anything, so zero is a
    // real answer and worth saying out loud rather than looking like a failure.
    if (!r.dispatched_to) {
      alert("No connected agent has a discovery range. Add one to the manifest " +
            "under \"discovery\" first.");
    }
  } catch (e) {
    btn.textContent = "Failed";
    alert(e.message);
  }
  setTimeout(() => { btn.disabled = false; btn.textContent = was; refresh(); }, 60000);
}

async function loadLogs() {
  let d;
  try {
    d = await api("/api/logs");
  } catch (e) {
    return;
  }
  $("log-sub").textContent = d.receiving
    ? `keeping ${d.retain_hours}h`
    : "not switched on";
  const empty = $("log-empty");
  empty.hidden = (d.senders || []).length > 0;
  if (!empty.hidden) {
    empty.innerHTML = d.receiving
      ? `Nothing has sent anything yet. Point a device's syslog at
         <code>${esc(location.hostname)}:514</code>.`
      : `The receiver is off. Start the portal with
         <code>--syslog-bind 0.0.0.0:514</code> to accept device logs.`;
  }
  // What the badge is counting, said out loud. A red number with no visible
  // cause is the thing this whole dashboard is supposed not to do.
  setHTML($("log-silent"), (d.asked_but_silent || []).map((m) => `<div class="need t3">
    <div class="rule"></div>
    <div class="what">
      <div><b class="mono">${esc(m.machine)}</b> was asked to forward
        ${esc(m.min_severity)} and worse ${m.asked_at ? ago(m.asked_at) : ""}, and nothing
        has arrived.</div>
      <div class="tail">${m.online
        ? `It is connected, so either it genuinely has nothing at ${esc(m.min_severity)}
           level, or the forwarding is broken. A quiet host can hold ten such lines
           a day &mdash; use <b>Estimate volume</b> on its page to see which.`
        : "It is offline, so this will stay true until it comes back."}</div>
    </div>
    <span class="msg">asked, silent</span>
  </div>`).join(""));

  setHTML($("log-senders"), (d.senders || []).map((s) => {
    // What is being kept, and who decided the level. Both belong on the row:
    // "42 lines" means something different at `warning` than at `info`, and a
    // retention nobody can see is a retention nobody trusts.
    const level = s.set_here
      ? `<b>${esc(s.min_severity)}</b> and worse, set here`
      : `level set on the device`;
    return `<div class="reading">
      <span class="k">${esc(s.device || "unknown sender")}</span>
      <span class="v">${esc(s.source)}</span>
      <span class="since">${(s.lines || 0).toLocaleString()} lines &middot; ${size(s.bytes)} &middot; ${
        s.last_line_at ? ago(s.last_line_at) : "quiet"}
        <button class="act" style="padding:1px 8px;font-size:11.5px;margin-left:8px"
          onclick="showLog(${jsq(s.source)})">Read</button></span>
      <span class="k" style="grid-column:1/-1;font-size:11.5px">
        ${s.set_here
          // Changing the level is a command to that machine, so it is only
          // offered where PatchPanel actually drives the sender. An appliance
          // decides its own level, and a control that silently did nothing
          // would be worse than no control.
          ? `forwarding
             <select class="fld" style="font-size:11px;padding:1px 4px"
               title="What this machine sends. Changing it dispatches to the agent."
               onchange="setLogLevel(${jsq(s.source)}, this.value)">
               ${["err", "warning", "notice", "info"].map((v) =>
                 `<option value="${v}"${v === s.min_severity ? " selected" : ""}>${
                   v === "err" ? "errors" : v === "warning" ? "warnings"
                   : v === "notice" ? "notices" : "everything"} and worse</option>`).join("")}
             </select>`
          : `<span title="This sender pushes syslog; the level lives in its own configuration.">level set on the device</span>`}
        &middot; keep
        <select class="fld" style="font-size:11px;padding:1px 4px"
          title="How long these lines are kept before being discarded. Shortening it takes effect immediately."
          onchange="setLogRetention(${jsq(s.source)}, this.value)">
          ${[["6","6 hours"],["24","1 day"],["72","3 days"],["168","7 days"]].map(([v, t]) =>
            `<option value="${v}"${Number(v) === (s.retain_hours || 24) ? " selected" : ""}>${t}</option>`).join("")}
        </select>
      </span>
    </div>`;
  }).join(""));
}

async function setLogLevel(source, min_severity) {
  try {
    await api("/api/logs/" + encodeURIComponent(source) + "/level", {
      method: "POST",
      body: JSON.stringify({ min_severity }),
    });
    refresh();
  } catch (e) {
    alert(e.message);
    refresh();
  }
}

// Shortening this throws lines away, so it says how many before doing it.
async function setLogRetention(source, hours) {
  const h = Number(hours);
  try {
    const before = await api("/api/logs/" + encodeURIComponent(source) + "?limit=1");
    if (!confirm(`Keep ${h} hour(s) of ${source}?

` +
        `${(before.total || 0).toLocaleString()} line(s) are held now; anything older ` +
        `than ${h} hour(s) is discarded immediately.`)) { refresh(); return; }
    await api("/api/logs/" + encodeURIComponent(source) + "/retention", {
      method: "POST",
      body: JSON.stringify({ hours: h }),
    });
    refresh();
  } catch (e) {
    alert(e.message);
    refresh();
  }
}

async function showLog(source, contains) {
  const box = $("log-view");
  const q = contains === undefined ? ($("log-filter") ? $("log-filter").value : "") : contains;
  const limit = $("log-limit") ? $("log-limit").value : "400";
  const d = await api("/api/logs/" + encodeURIComponent(source) +
    "?limit=" + encodeURIComponent(limit) + "&contains=" + encodeURIComponent(q || ""));
  box.hidden = false;
  // Say what this is a slice of. Showing the last 400 lines without mentioning
  // that there are 12,000 invites the reader to believe they have seen the lot,
  // which is how you conclude a thing never happened.
  const shown = d.lines.length;
  const total = d.total || shown;
  const scope = q
    ? `${shown} line(s) matching ${esc(q)}${shown >= Number(limit) ? ` (capped at ${esc(limit)})` : ""}`
    : `last ${shown} of ${total} line(s) held`;
  setHTML(box, `<h2>${esc(d.device || source)} <span class="sub">${esc(source)}</span>
      <button class="act" style="float:right;padding:2px 8px;font-size:11.5px"
        onclick="$('log-view').hidden = true">Close</button></h2>
    <div class="step">
      <div class="bar">
        <input id="log-filter" placeholder="only lines containing&hellip;" spellcheck="false"
          value="${esc(q || "")}"
          onkeydown="if (event.key === 'Enter') showLog(${jsq(source)})"
          style="flex:1;padding:7px 10px;border:1px solid var(--line);border-radius:6px;background:var(--bg);color:var(--ink);font-size:12.5px">
        <select id="log-limit" class="fld" onchange="showLog(${jsq(source)})"
          title="How many of the most recent lines to show.">
          ${["200", "400", "2000", "5000"].map((v) =>
            `<option value="${v}"${v === String(limit) ? " selected" : ""}>last ${v}</option>`).join("")}
        </select>
        <button class="act" onclick="exportLog(${jsq(source)})"
          title="Download everything held for this sender, matching the filter if one is set.">Export</button>
      </div>
      <div class="msg" style="margin-top:6px">${scope}, newest last</div>
      <pre style="max-height:420px">${
        d.lines.map(esc).join("&#10;") || "(nothing)"}</pre>
    </div>`);
  box.scrollIntoView({ behavior: "smooth", block: "nearest" });
}

// Fetched rather than linked, so the bearer token goes with it: a plain href
// would work only on a portal running without authentication.
async function exportLog(source) {
  const q = $("log-filter") ? $("log-filter").value : "";
  const url = "/api/logs/" + encodeURIComponent(source) +
    "/export?contains=" + encodeURIComponent(q || "");
  try {
    const r = await fetch(url, { headers: TOKEN ? { Authorization: "Bearer " + TOKEN } : {} });
    if (!r.ok) throw new Error("the portal answered " + r.status);
    const blob = await r.blob();
    const name = (r.headers.get("content-disposition") || "")
      .match(/filename="?([^"]+)"?/)?.[1] || source + ".log";
    const a = document.createElement("a");
    a.href = URL.createObjectURL(blob);
    a.download = name;
    document.body.appendChild(a);
    a.click();
    a.remove();
    // Revoked on the next tick; doing it immediately cancels the download in
    // some browsers.
    setTimeout(() => URL.revokeObjectURL(a.href), 10000);
  } catch (e) {
    alert("Could not export: " + e.message);
  }
}

async function loadDevices() {
  const d = await api("/api/devices");
  const eol = d.devices.filter((x) => x.eol);
  const warn = $("devices-eol");
  warn.hidden = eol.length === 0;
  if (eol.length) {
    warn.innerHTML = `<div class="step"><div class="note" style="border-color:var(--bad)">
      <b>${eol.length} device(s) are running a release that is end of life.</b>
      They report nothing pending, and that is true: there will be no more updates for them,
      including for anything found after today. The fix is a release upgrade, not a patch run.
      <ul style="margin:8px 0 0 18px">${eol.map((x) => `<li>
        <b>${esc(x.label || x.id)}</b> &mdash; ${esc(x.firmware || "unknown version")}${
          x.eol_note ? `. ${esc(x.eol_note)}` : ""}${
          x.stale ? " (not answering; this was true as of the last probe that worked)" : ""}</li>`
        ).join("")}</ul>
    </div></div>`;
  }

  $("devices-empty").hidden = d.devices.length > 0;
  setHTML($("device-rows"), d.devices.map((x) => {
    // One line that says what to do about it, rather than four columns that
    // each say part of it.
    const state = !x.probed
      ? { cls: "", pill: '<span class="pill">not probed yet</span>' }
      : !x.reachable
        ? { cls: "bad", pill: '<span class="pill bad">unreachable</span>' +
            (x.eol ? ' <span class="pill bad" title="Still true whether or not it answered this probe.">end of life</span>' : "") +
            (x.updates_known && x.updates ? ` <span class="pill warn">${x.updates} update(s)</span>` : "") }
        : x.eol
          ? { cls: "bad", pill: `<span class="pill bad" title="${esc(x.eol_note || "No further updates will be released for it.")}">end of life</span>` }
          : x.updates_known && x.updates
            ? { cls: "warn", pill: `<span class="pill warn">${x.updates} update(s)</span>` }
            : x.drift
              ? { cls: "warn", pill: '<span class="pill warn">drift</span>' }
              : x.updates_known
                ? { cls: "", pill: '<span class="pill ok">up to date</span>' }
                : { cls: "", pill: '<span class="pill">reachable</span>' };

    return `<div class="dev ${state.cls}">
      <h3>
        <input class="namefld" value="${esc(x.label)}" spellcheck="false"
          placeholder="${esc((x.id || "").split(".")[0])}"
          title="Click to rename."
          onchange="renameDevice(${jsq(x.id)}, this.value)"
          onkeydown="if (event.key === 'Enter') this.blur()">
        ${x.url ? `<a href="${esc(x.url)}" target="_blank" rel="noreferrer noopener"
          class="act" style="padding:2px 8px;font-size:11.5px;text-decoration:none"
          title="Open ${esc(x.url)}">Open</a>` : ""}
      </h3>
      <div class="mono msg">${esc(x.target)}</div>

      <div class="ver${x.stale ? " stale" : ""}"
        title="${x.stale ? "Last confirmed " + esc(x.last_good_at || "") : ""}">${
        esc(x.firmware || "\u2014")}</div>
      ${x.stale ? `<div class="msg">remembered from ${ago(x.last_good_at)} &mdash;
        the last probe did not get an answer</div>` : ""}
      <div>${state.pill}${x.reboot_required ? ' <span class="pill warn">reboot</span>' : ""}
        ${x.expect_version ? `<span class="msg">expected ${esc(x.expect_version)}</span>` : ""}</div>
      ${x.error ? `<div class="msg bad" style="margin-top:6px">${esc(x.error)}</div>` : ""}
      ${x.detail ? `<details data-k="dev-${esc(x.id)}" style="margin-top:8px"><summary class="msg">what it said</summary>
        <pre style="max-height:180px">${esc(x.detail)}</pre></details>` : ""}

      <input class="namefld mono" value="${esc(x.url)}" spellcheck="false"
        placeholder="management URL" style="font-size:11.5px;margin-top:8px"
        title="Where to go to act on this device."
        onchange="setDeviceUrl(${jsq(x.id)}, this.value)"
        onkeydown="if (event.key === 'Enter') this.blur()">

      ${x.auto_update !== undefined && x.auto_update !== null ? `<div style="margin-top:8px">
        <label class="msg">PatchPanel installs</label>
        <select class="fld" style="margin-left:6px;font-size:11.5px;padding:3px 6px"
          onchange="setAutoUpdate(${jsq(x.id)}, this.value)">
          <option value="off"${x.auto_update === "off" ? " selected" : ""}>nothing &mdash; report only</option>
          <option value="software"${x.auto_update === "software" ? " selected" : ""}>its software and add-ons</option>
          <option value="everything"${x.auto_update === "everything" ? " selected" : ""}>software and device firmware</option>
        </select>
      </div>` : ""}

      <div class="foot">
        <span class="msg" title="via ${esc(x.collector_host)}${x.latency_ms != null ? `, ${x.latency_ms}ms` : ""}">
          ${x.probed ? `checked ${ago(x.checked_at)}` : "never checked"}</span>
        ${x.collectors > 1 ? `<span class="pill warn" title="No collector is named for this device, so every agent in the site probes it.">+${x.collectors - 1} collectors</span>` : ""}
        <span style="margin-left:auto"></span>
        <button class="act" style="padding:2px 8px;font-size:11.5px"
          onclick="deviceHistory(${jsq(x.id)})">History</button>
        <button class="act" style="padding:2px 8px;font-size:11.5px"
          onclick="probeDevice(${jsq(x.collector)}, ${jsq(x.id)})">Probe</button>
        <button class="act" style="padding:2px 8px;font-size:11.5px"
          onclick="removeDevice(${jsq(x.id)})">Remove</button>
      </div>
    </div>`;
  }).join(""));

  renderNetwork(d);
}

// The last device payload, kept so expanding a discovered row can redraw the
// table from what it was already built from instead of asking the portal again.
let LAST_DEVICES = null;

// Show only the hosts nothing accounts for.
//
// Off by default: the whole point of listing the fleet's own machines here is
// that "what is on this network" is answerable in one place. The filter exists
// because once the answer is forty rows, finding the four that are a mystery is
// the common follow-up question.
let ONLY_UNKNOWN = false;

function onlyUnknown(on) {
  ONLY_UNKNOWN = on;
  if (LAST_DEVICES) renderNetwork(LAST_DEVICES);
}

function renderNetwork(d) {
  LAST_DEVICES = d;
  const all = d.network || [];
  const unknown = all.filter((h) => !h.known);
  const hosts = ONLY_UNKNOWN ? unknown : all;
  $("network-empty").hidden = hosts.length > 0;
  if (!hosts.length) {
    setHTML($("network-empty"), all.length
      ? `All ${all.length} host(s) that answered are accounted for &mdash; every one is a
         machine in this fleet or a declared appliance. Nothing is unexplained.`
      : `Nothing has answered a sweep yet. Add a <code>discovery</code> range to the
         manifest, and the collector for its site will sweep it on its own schedule.`);
  }

  // How old the list is, said plainly. A page that shows hosts with no age on
  // them invites the reader to assume it is live, and a sweep is the one thing
  // here that is emphatically not - it runs twice an hour at most.
  const sweeps = d.sweeps || [];
  const every = d.sweep_every_secs ? `, automatically every ${dur(d.sweep_every_secs)}` : "";
  const stale = sweeps.length > 1
    ? ` <span class="msg">(${esc(sweeps[sweeps.length - 1].collector_host)} last swept
        ${ago(sweeps[sweeps.length - 1].at)})</span>`
    : "";
  setHTML($("sweep-age"), `<div class="step">
    <div class="msg">${sweeps.length
      ? `Swept ${ago(sweeps[0].at)} by ${esc(sweeps[0].collector_host)}${every}.${stale}`
      : `No sweep has finished yet${every ? every.replace(", a", "; a") : ""}. The list below
         is whatever was found before this portal last restarted, if anything.`}</div>
    <label class="msg" style="display:block;margin-top:6px;cursor:pointer">
      <input type="checkbox" onchange="onlyUnknown(this.checked)"${ONLY_UNKNOWN ? " checked" : ""}>
      Only the ${unknown.length} nothing accounts for
      <span class="msg">(of ${all.length} answering)</span>
    </label>
  </div>`);

  // A range asking for nmap on a collector that has none still finds the hosts,
  // and would otherwise render as a network where nothing could be identified.
  setHTML($("network-note"), (d.discovery_notes || []).map((n) => `<div class="step">
      <div class="note" style="border-color:var(--warn)">${esc(n)}</div>
    </div>`).join(""));

  setHTML($("network-rows"), hosts.map((h) => {
    const svc = h.services || [];
    // One pill per row, and it is the row's verdict. A machine or a declared
    // appliance gets no pill at all - it is simply named - because "this is
    // accounted for" is the unremarkable case and marking it competes with the
    // rows that are actually asking for attention. Amber, not red: an
    // unexplained host is worth a look, not an alarm.
    // For an unexplained host, whatever the scan managed to establish about
    // what it is: the reverse-DNS name first because somebody wrote that down,
    // then nmap's device class, then the hint. "unexplained · printer ·
    // brother.lan" is a row somebody can act on; "unexplained" alone is one
    // they have to go and investigate.
    const id = h.identity || {};
    // The reverse-DNS name, and whatever the scan managed to name the thing.
    //
    // nmap's device class is deliberately NOT here, though it was: on this
    // network it calls three MoCA adapters "printer" and an iMac "phone", both
    // at 99% accuracy. In a table cell that reads as a finding, and a wrong
    // finding is worse than a blank - so it stays in the expansion, next to the
    // sentence explaining it is a guess of the same standing as the OS match.
    const named = svc.some((s) => s.product) ? "" : h.hint;
    // `dotted` drops the repeat when the hint has fallen back to the PTR name,
    // which it does whenever nothing better was established.
    const said = h.known ? "" : dotted([(id.hostnames || [])[0], named]);
    const what = h.known
      ? `${esc(h.known.name)} <span class="msg">${h.known.role}${
          h.known.via === "reverse DNS" ? ", by name" : ""}</span>`
      : `<span class="pill warn">unexplained</span>${
          said ? ` <span class="msg">${esc(said)}</span>` : ""}`;
    return `<tr class="clicky${OPEN_HOST === h.ip ? " open" : ""}"
        onclick="toggleHost(${jsq(h.ip)})"
        title="Everything the sweep learned about this host.">
      <td class="mono">${esc(h.ip)}</td>
      <td class="what">${what}</td>
      <td>${esc((h.identity || {}).mac_vendor) || '<span class="msg">-</span>'}</td>
      <td class="mono">${[
        h.open_ports.join(", "),
        (h.open_udp || []).length
          ? `<span class="msg">${h.open_udp.join(", ")} udp</span>`
          : "",
      ].filter((x) => x).join(" ")}</td>
      <td class="svc">${svc.length ? svc.map((s) => `<div class="msg"><span class="mono">${s.port}</span>
        ${esc([s.name, s.product, s.version].filter((x) => x).join(" "))}</div>`).join("")
        : `<span class="msg">-</span>`}</td>
      <td>${esc(h.collector_host)}
        <span class="msg">${h.scanner === "nmap" ? "nmap" : "tcp sweep"}</span></td>
    </tr>${OPEN_HOST === h.ip ? hostDetail(h, 6) : ""}`;
  }).join(""));
}

// The portal cannot know which address you reached it on (it may be bound to
// 0.0.0.0), so the commands are built from the URL in your address bar - which
// is by definition an address that works.
async function loadAdd() {
  if (!$("dev-hint").textContent) deviceKindChanged();
  const site = ($("add-site").value || "default").replace(/[^A-Za-z0-9._-]/g, "");
  const host = $("add-portal").value.trim() || location.host;
  let token = "<enrollment-token>";
  try {
    token = (await api("/api/enrollment")).token || token;
  } catch (e) {
    // Leave the placeholder; the shape of the command is still useful.
  }

  $("cmd-linux").textContent =
    `curl -fsSL http://${host}/install.sh | sh -s --` +
    `${AUTH_REQUIRED ? ` --token ${token}` : ""} --site ${site}`;

  // One line, no backslashes, no scriptblock, and no token to paste: the
  // agent asks the portal for it. `./` works in PowerShell and cannot be
  // mangled the way `.\` can.
  const needsToken = AUTH_REQUIRED ? ` --token ${token}` : "";
  $("cmd-win").textContent =
    `irm http://${host}/download/pp-agent.exe -OutFile pp-agent.exe; ` +
    `./pp-agent.exe setup --portal ${host}${needsToken} --site ${site}`;

  // A bare IP works until DHCP moves the portal, and then every enrolled agent
  // is pointing at nothing. Say so once, here, rather than in a runbook.
  const hint = $("add-hint");
  if (/^\d+\.\d+\.\d+\.\d+(:\d+)?$/.test(host)) {
    hint.textContent =
      "This is an IP address. If this portal is on DHCP, prefer its hostname " +
      "so agents keep working when the address changes.";
    hint.className = "msg bad";
  } else {
    hint.textContent = "Agents will connect to this address, so it must resolve from every machine you enrol.";
    hint.className = "msg";
  }
}

// The async clipboard API needs a secure context, and this portal is plain
// HTTP, so fall back to the old selection trick rather than silently failing.
function copyText(text, el) {
  const flash = () => {
    const o = el.textContent;
    el.textContent = "copied";
    setTimeout(() => (el.textContent = o), 900);
  };
  if (navigator.clipboard && window.isSecureContext) {
    navigator.clipboard.writeText(text).then(flash);
    return;
  }
  const ta = document.createElement("textarea");
  ta.value = text;
  ta.style.position = "fixed";
  ta.style.opacity = "0";
  document.body.appendChild(ta);
  ta.select();
  try { document.execCommand("copy"); flash(); } catch (e) { /* select manually */ }
  document.body.removeChild(ta);
}

function copyCmd(id) {
  const text = $(id).textContent;
  const done = (btn) => { const o = btn.textContent; btn.textContent = "Copied"; setTimeout(() => (btn.textContent = o), 1200); };
  const btn = $(id).parentElement.querySelector("button");
  if (navigator.clipboard && window.isSecureContext) {
    navigator.clipboard.writeText(text).then(() => done(btn));
    return;
  }
  const ta = document.createElement("textarea");
  ta.value = text;
  ta.style.position = "fixed";
  ta.style.opacity = "0";
  document.body.appendChild(ta);
  ta.select();
  try { document.execCommand("copy"); done(btn); } catch (e) { /* user can select manually */ }
  document.body.removeChild(ta);
}

async function loadManifest() {
  const m = await api("/api/manifest");
  $("manifest-doc").value = JSON.stringify(m, null, 2);
  $("manifest-msg").textContent = "revision " + m.revision;
  $("manifest-msg").className = "msg";

  const builds = await api("/api/builds");
  $("builds-empty").hidden = builds.length > 0;
  $("build-rows").innerHTML = builds.map((b) => `<tr>
      <td class="mono">${esc(b.version)}</td><td>${esc(b.os)}</td><td class="mono">${esc(b.arch)}</td>
      <td class="mono">${esc(b.url)}</td><td class="mono">${esc(b.sha256.slice(0, 16))}…</td>
    </tr>`).join("");
}

async function saveManifest() {
  const msg = $("manifest-msg");
  let doc;
  try {
    doc = JSON.parse($("manifest-doc").value);
  } catch (e) {
    msg.textContent = "Invalid JSON: " + e.message;
    msg.className = "msg bad";
    return;
  }
  try {
    const r = await api("/api/manifest", { method: "PUT", body: JSON.stringify(doc) });
    msg.textContent = `Published revision ${r.revision} to ${r.pushed_to} connected agent(s).`;
    msg.className = "msg ok";
    loadManifest();
  } catch (e) {
    msg.textContent = e.message;
    msg.className = "msg bad";
  }
}

async function loadActivity() {
  const rows = await api("/api/commands?limit=40");
  $("commands-empty").hidden = rows.length > 0;
  const host = (id) => (AGENTS.find((a) => a.id === id) || {}).hostname || id.slice(0, 8);
  const html = rows.map((c) => {
    const running = c.ok === null || c.ok === undefined;
    const state = running
      ? '<span class="pill">running</span>'
      : (c.ok ? '<span class="pill ok">ok</span>' : '<span class="pill bad">failed</span>');
    const body = (c.detail || c.progress || "").trim();
    return `<div style="padding:10px 14px;border-bottom:1px solid var(--line)">
      <div>${state} <b>${esc(c.kind)}</b> on ${esc(host(c.agent_id))}
        <span class="msg">· ${ago(c.created_at)}</span><span class="mono msg cmdid" title="${esc(c.id)} - click to copy" onclick="copyText('${esc(c.id)}', this)">${esc(c.id.slice(0, 8))}</span></div>
      ${c.summary ? `<div class="mono" style="margin-top:4px">${esc(c.summary)}</div>` : ""}
      ${body ? `<details data-k="${esc(c.id)}" ontoggle="tailLog(this)"><summary>output</summary>
        <pre data-k="${esc(c.id)}" data-live="${running ? 1 : 0}">${esc(body)}</pre></details>` : ""}
    </div>`;
  }).join("");
  setHTML($("commands"), html);
}

// Patching restarts services and can demand a reboot, so it is the one
// per-machine action that asks first.
async function restartAgent(id) {
  if (!confirm("Restart the agent on this machine? It re-detects package backends and reconnects in a few seconds. Nothing else on the machine is touched.")) return;
  await cmd(id, "restart_agent");
}

// Per machine, never fleet-wide and never from the manifest. This writes to
// /etc/rsyslog.d and reloads a service, and a change to a machine gets asked
// for rather than applied because a document said so.
// What this machine would actually send, before committing to a level.
//
// The spread between levels is enormous and invisible otherwise: one host here
// holds ten warning-or-worse lines a day and 2,794 at info. Choosing blind is
// how somebody ends up either shipping a firehose or shipping nothing.
async function estimateVolume(id) {
  const out = $("vol-out");
  if (out) out.textContent = "Counting the last 24 hours…";
  try {
    const r = await api(`/api/agents/${id}/commands`, {
      method: "POST",
      body: JSON.stringify({ command: { kind: "journal_volume" } }),
    });
    // The count runs on the machine, so the answer arrives with the command
    // result rather than from this call.
    for (let i = 0; i < 20; i++) {
      await new Promise((r) => setTimeout(r, 1500));
      const log = await api(`/api/agents/${id}`);
      const hit = (log.commands || []).find((c) => c.id === r.id);
      if (hit && hit.ok !== null && hit.ok !== undefined) {
        if (out) out.textContent = hit.summary || "no answer";
        return;
      }
    }
    if (out) out.textContent = "Still counting; check the machine's activity.";
  } catch (e) {
    if (out) out.textContent = e.message;
  }
}

async function setForwarding(id, enable) {
  const sev = $("fwd-sev") ? $("fwd-sev").value : "warning";
  if (enable && !confirm(
      "Forward this machine's journal (" + sev + " and worse) to this portal? " +
      "Nothing is installed and nothing is written to the machine.")) return;
  try {
    await api(`/api/agents/${id}/commands`, {
      method: "POST",
      body: JSON.stringify({
        command: { kind: "configure_syslog", enable, min_severity: sev },
      }),
    });
    // The state shown comes from the machine's own next scan, not from this
    // call succeeding - the file on disk is the truth, not the dispatch.
    alert(enable
      ? "Asked. It shows as forwarding once the machine next reports."
      : "Asked. The rule is removed and kept beside itself as a backup.");
    refresh();
  } catch (e) {
    alert(e.message);
  }
}

async function installPrereqs(id) {
  const warn = "Install the missing update tooling on this machine?\n\n" +
    "On Linux this installs fwupd, so firmware is looked at at all. On Windows it "  +
    "downloads PSWindowsUpdate and the WinGet client and repairs winget for all users.\n\n" +
    "Restart the agent afterwards so the new backend is detected - they are found "  +
    "once at startup.";
  if (!confirm(warn)) return;
  await cmd(id, "install_prerequisites");
}

async function fullUpgrade(id) {
  const warn = "Run a FULL upgrade? This can install new packages and remove existing ones. It is how held-back upgrades such as a kernel get applied.";
  if (!confirm(warn)) return;
  await cmd(id, "apply_patches", { full: true });
}

async function cleanupNow(id) {
  if (!confirm("Remove packages nothing depends on, and empty the package cache?")) return;
  await cmd(id, "cleanup");
}

async function patchNow(id, host) {
  if (!confirm(`Install pending OS updates on ${host}?

This can restart services and may require a reboot.`)) return;
  await cmd(id, "apply_patches");
}

async function cmd(id, kind, extra = {}) {
  // Latch before awaiting: the click that starts the request is the one we
  // need to stop happening twice.
  JUST_SENT.set(id, Date.now());
  refresh();
  try {
    const res = await api(`/api/agents/${id}/commands`, {
      method: "POST",
      body: JSON.stringify({ command: { kind, ...extra } }),
    });
    setTimeout(refresh, 400);
    return res;
  } catch (e) {
    JUST_SENT.delete(id);
    refresh();
    alert(e.message);
  }
}

async function broadcast(kind) {
  // The two broadcast bars live on different tabs now, so the result has to
  // land under the button that was actually pressed.
  const msg = kind === "apply_manifest" ? $("broadcast-msg2") : $("broadcast-msg");
  if (kind === "apply_manifest" &&
      !confirm("Install, upgrade or remove applications on every connected machine so it " +
               "matches the published manifest. This changes systems. Continue?")) return;
  try {
    const r = await api("/api/commands/broadcast", {
      method: "POST",
      body: JSON.stringify({ command: { kind } }),
    });
    msg.textContent = `Sent to ${r.dispatched_to} agent(s).`;
    msg.className = "msg ok";
  } catch (e) {
    msg.textContent = e.message;
    msg.className = "msg bad";
  }
}

// What needs a person, grouped by cause.
//
// The server decides what belongs here and how to word it; this only draws it.
// That is deliberate - the badge, the list and the empty state all have to
// agree, and they cannot if two of them count for themselves.
function drawNeeds(d) {
  const items = d.attention || [];
  const g = d.gist || {};

  setHTML($("needs"), items.map((n, i) => {
    const who = n.who && n.who.length
      ? `<div class="who" id="who-${i}" hidden>${n.who.map((w) =>
          `<div><b>${w.link
            ? `<a href="${esc(w.link)}" style="color:inherit">${esc(w.name)}</a>`
            : esc(w.name)}</b><span>${esc(w.context)}</span><span>${esc(w.why)}</span></div>`
        ).join("")}</div>`
      : "";
    // The names ship with the item, so opening them is free - grouping costs
    // vertical space and nothing else.
    const count = n.who && n.who.length
      ? ` <button class="disc" data-who="${i}" data-n="${n.who.length}"
          title="show which ones">${n.who.length} &#9662;</button>`
      : "";
    return `<div class="need t${n.tier}"><div class="rule"></div><div class="what">
      <div>${esc(n.say)}${count}</div>
      ${n.tail ? `<div class="tail">${esc(n.tail)}</div>` : ""}${who}</div>
      <button class="btn" onclick="showTab('${esc(n.link.replace("#", ""))}')">${esc(n.action)} &rarr;</button></div>`;
  }).join(""));

  $("needs-sub").textContent = items.length
    ? `${items.length} thing(s) - everything else is on a schedule`
    : "";

  // An empty state has to be a proof, not a reassurance. "All good" is exactly
  // what a dashboard that has stopped checking would also say.
  const none = $("needs-none");
  none.hidden = items.length > 0;
  if (!items.length) {
    none.innerHTML = `<b>Nothing needs you.</b> ${g.reporting || 0} of ${g.machines || 0}
      machines reporting, none with a scan problem, and
      ${g.covered ? `${g.covered} pending update(s) belong to a pool that will take them` :
        "nothing pending anywhere"}.`;
  }

  setHTML($("gist"), [
    `<b>${g.machines || 0}</b> machines, <b>${g.reporting || 0}</b> reporting`,
    `<b>${g.pending || 0}</b> updates to install, <b>${g.security || 0}</b> security`,
    g.covered ? `<b>${g.covered}</b> of them belong to a pool` : "",
    `<a href="#machines" onclick="showTab('machines');return false">all machines &rarr;</a>`,
  ].filter(Boolean).map((t) => `<span>${t}</span>`).join(""));
}

// One delegated handler: the rows are redrawn every few seconds, so a listener
// per button would be attached and dropped on every refresh.
document.addEventListener("click", (e) => {
  const d = e.target.closest(".disc");
  if (!d) return;
  const box = $("who-" + d.dataset.who);
  if (!box) return;
  box.hidden = !box.hidden;
  d.innerHTML = d.dataset.n + (box.hidden ? " &#9662;" : " &#9652;");
});

async function refresh() {
  // Assume nothing is paused; whichever render is blocked will say so.
  showPaused(false);
  try {
    // The fleet call also populates the hostname lookup the activity tab uses.
    await loadFleet();
    if (TAB === "machines") await loadDevices();
    if (TAB === "logs") await loadLogs();
    // The unmanaged list arrives with the device list, so the same call fills it.
    if (TAB === "discovered") await loadDevices();
    if (TAB === "schedule") await loadPools();
    if (TAB === "jobs") await loadJobs();
    if (TAB === "backups") await loadBackups();
    if (TAB === "setup") {
      if (!$("add-portal").value) $("add-portal").value = location.host;
      await loadAdd();
      if (!$("manifest-doc").value) await loadManifest();
      await loadActivity();
    }
    if (TAB === "agent") await loadAgent(ROUTE.id);
  } catch (e) {
    if (e.message !== "unauthorized") console.error(e);
  }
}

// The portal can be run with authentication turned off for a trusted
// network; ask it before deciding whether to demand a token.
async function boot() {
  try {
    const mode = await fetch("/api/auth-mode").then((r) => r.json());
    AUTH_REQUIRED = mode.required !== false;
  } catch (e) {
    // Unreachable or an older portal: assume auth is on rather than
    // silently dropping the token from requests.
  }
  if (!AUTH_REQUIRED || TOKEN) {
    $("app").hidden = false;
    showTab(TAB);
  } else {
    gate();
    $("gate-msg").className = "msg";
  }
}
boot();
// Polling covers both entry paths, and skips itself while the gate is up.
// ---------------------------------------------------------------------------
// The live feed
//
// Lines arrive from the portal in lumps: one poll every two seconds, carrying
// whatever happened in between - on this fleet a single firewall can make that two
// hundred lines. Putting a lump into the DOM and then animating the scrollbar
// across it was the wrong shape, and no amount of easing fixed it, because the
// jump was the arrival and not the scrolling.
//
// So the lump is never shown as a lump. Received lines go into a queue, and a
// frame loop takes a few off the front at a time, which means the feed grows by a
// row or two per frame while sitting at the bottom. Nothing has to travel: the
// scroll stays where it already is, and the content coming up from underneath is
// the movement. A row fades as it lands, and that is the only animation left.
// ---------------------------------------------------------------------------

let LIVE_ON = false;
let LIVE_CURSOR = 0;
let LIVE_TIMER = null;
// Everything the page is holding, rendered or not. The filter re-renders from
// this, so it has to include what is still queued.
let LIVE_LINES = [];
// Received but not yet in the DOM. Drained by dripLive, a few per frame.
let LIVE_QUEUE = [];
let LIVE_RAF = null;
let LIVE_MISSED = 0;
let LIVE_WHO = [];
// Set when the reader has scrolled up. Following is the default, but the moment
// somebody scrolls back to look at something, moving the view under them is the
// rudest thing this page could do.
let LIVE_HELD = false;
// Whether they stopped it by hand, which the tab-change autostart respects.
let LIVE_STOPPED = false;
// When they last actually touched the feed, and the scroll offset this code last
// wrote. Between them these separate the reader's scrolling from our own: every
// frame that pins the feed to the bottom emits a scroll event that is, at the
// event, indistinguishable from a drag of the scrollbar.
let LIVE_GESTURE = 0;
let LIVE_SET_TO = -1;

// Whether the reader has asked for less movement. Read once: it is consulted on
// every frame, and a media query lookup per frame is work for an answer that
// almost never changes.
const REDUCED_MOTION = typeof matchMedia === "function"
  && matchMedia("(prefers-reduced-motion: reduce)").matches;

// What this window keeps.
//
// Far smaller than the portal's ring on purpose. Every line is five elements once
// rendered, so three thousand of them is fifteen thousand nodes, and a fleet with
// a chatty firewall fills that in about two minutes - at which point the whole tab
// is slow, not just this card. What scrolls off here is still in the per-sender
// files, so keeping less costs nearly nothing and keeping more costs the page.
const LIVE_MAX = 800;

// Rows added per frame, and the queue length above which the feed stops trying to
// show every line.
//
// At sixty frames a second, four per frame is two hundred and forty lines a
// second, which is comfortably more than this fleet produces - so in practice the
// queue drains as fast as it fills and the delay between a line arriving and being
// seen stays under a second. The ceiling is for the pathological case: if
// something starts emitting thousands of lines a second, dripping them all would
// put the feed minutes behind the truth, which is worse than admitting it skipped
// some. LIVE_SKIPPED says so on screen.
const LIVE_PER_FRAME = 4;
const LIVE_QUEUE_MAX = 1200;
let LIVE_SKIPPED = 0;

function toggleLive() {
  LIVE_ON = !LIVE_ON;
  if (!LIVE_ON) LIVE_STOPPED = true;
  $("live-toggle").textContent = LIVE_ON ? "Stop" : "Start";
  $("live-empty").hidden = LIVE_ON || LIVE_LINES.length > 0;
  if (LIVE_ON) {
    pollLive();
    // Faster than the page's 5s, because a feed that updates every five seconds
    // does not read as live. Only while the tab is open - see pollLive.
    LIVE_TIMER = setInterval(pollLive, 2000);
  } else {
    clearInterval(LIVE_TIMER);
    LIVE_TIMER = null;
    // Show what is already in hand rather than stopping mid-queue with lines the
    // reader can see the count of but not the text of.
    flushLive();
    liveState();
  }
}

async function pollLive() {
  // The tab can change under a running timer, and polling a feed nobody is
  // looking at is pure waste.
  if (!LIVE_ON || TAB !== "logs" || $("app").hidden) return;
  let r;
  try {
    r = await api(`/api/logs/live?after=${LIVE_CURSOR}`);
  } catch (e) {
    // A portal restart is the common case. Keep the lines already on screen and
    // say so, rather than clearing to a blank pane.
    liveState("reconnecting");
    return;
  }
  LIVE_CURSOR = r.next;
  LIVE_MISSED += r.missed || 0;
  if (r.receiver_on) {
    liveState();
  } else {
    liveState("the syslog receiver is off; only forwarded journals will appear");
  }

  if (!r.lines || !r.lines.length) {
    liveCount();
    return;
  }

  for (const l of r.lines) {
    LIVE_LINES.push(l);
    LIVE_QUEUE.push(l);
    if (!LIVE_WHO.includes(l.who)) LIVE_WHO.push(l.who);
  }
  if (LIVE_LINES.length > LIVE_MAX) LIVE_LINES.splice(0, LIVE_LINES.length - LIVE_MAX);
  // Falling behind. Drop the oldest of what has not been shown yet, not the
  // newest: a live view that is behind is worth less than one that is current.
  if (LIVE_QUEUE.length > LIVE_QUEUE_MAX) {
    LIVE_SKIPPED += LIVE_QUEUE.length - LIVE_QUEUE_MAX;
    LIVE_QUEUE.splice(0, LIVE_QUEUE.length - LIVE_QUEUE_MAX);
  }
  whoOptions();
  startDrip();
}

// ---------------------------------------------------------------------------
// Dripping the queue into the DOM

function startDrip() {
  if (LIVE_RAF !== null) return;
  // Somebody who asked for less movement gets the lines, just not the pacing.
  if (REDUCED_MOTION) {
    flushLive();
    return;
  }
  LIVE_RAF = requestAnimationFrame(dripLive);
}

/// Move a few queued lines into the feed, then ask for the next frame.
///
/// The whole smoothness of this comes from how little each frame does. Appending
/// four rows to a container already scrolled to its bottom moves the visible
/// content up by four rows, once, at the refresh rate - which is what continuous
/// motion is. There is no scroll animation here at all, and there was never a need
/// for one.
function dripLive() {
  LIVE_RAF = null;
  if (!LIVE_ON || TAB !== "logs" || $("app").hidden) {
    // Nobody is watching; keep the lines and stop burning frames.
    return;
  }

  const feed = $("live-feed");
  const shown = liveFilter();
  const take = LIVE_QUEUE.splice(0, LIVE_PER_FRAME).filter(shown);
  if (take.length) {
    feed.insertAdjacentHTML("beforeend", take.map((l) => liveRow(l, true)).join(""));
    trimFeed(feed);
    pinLive(feed);
  }
  liveCount();

  if (LIVE_QUEUE.length) {
    LIVE_RAF = requestAnimationFrame(dripLive);
  }
}

/// Show everything queued at once, for the cases where pacing is wrong: the
/// reader pressed Stop, a filter changed, or they have asked for less movement.
function flushLive() {
  if (LIVE_RAF !== null) {
    cancelAnimationFrame(LIVE_RAF);
    LIVE_RAF = null;
  }
  if (!LIVE_QUEUE.length) return;
  const feed = $("live-feed");
  const shown = liveFilter();
  const take = LIVE_QUEUE.filter(shown);
  LIVE_QUEUE = [];
  if (take.length) {
    feed.insertAdjacentHTML("beforeend", take.map((l) => liveRow(l, false)).join(""));
    trimFeed(feed);
    pinLive(feed);
  }
  liveCount();
}

/// Hold the feed at its bottom, remembering where we put it.
///
/// The offset is recorded so the scroll handler can tell this apart from the
/// reader moving the bar. Every frame of a drip writes scrollTop, and every one of
/// those arrives at the handler as an ordinary scroll event.
function pinLive(feed) {
  if (LIVE_HELD) return;
  LIVE_SET_TO = feed.scrollHeight - feed.clientHeight;
  feed.scrollTop = LIVE_SET_TO;
}

// ---------------------------------------------------------------------------

// Keep the machine list in step without disturbing a choice already made.
function whoOptions() {
  const sel = $("live-who");
  const want = ["", ...LIVE_WHO.slice().sort()];
  if (sel.options.length === want.length) return;
  const chosen = sel.value;
  sel.innerHTML = want
    .map((w) => `<option value="${esc(w)}">${w ? esc(w) : "every machine"}</option>`)
    .join("");
  sel.value = chosen;
}

// Read the three controls once and return a predicate.
//
// Reading them inside the filter meant three DOM lookups per line - for values
// that cannot change while a batch is being rendered.
function liveFilter() {
  const text = $("live-filter").value.trim().toLowerCase();
  const who = $("live-who").value;
  const sev = Number($("live-sev").value);
  return (l) => {
    if (who && l.who !== who) return false;
    if (l.severity > sev) return false;
    if (text && !(`${l.who} ${l.tag} ${l.msg}`.toLowerCase().includes(text))) return false;
    return true;
  };
}

function liveRow(l, fresh) {
  const cls = (l.severity <= 3 ? " err" : l.severity <= 4 ? " warn" : "")
    + (fresh ? " fresh" : "");
  const t = new Date(l.at);
  const hh = String(t.getHours()).padStart(2, "0");
  const mm = String(t.getMinutes()).padStart(2, "0");
  const ss = String(t.getSeconds()).padStart(2, "0");
  return `<div class="row${cls}"><span class="t">${hh}:${mm}:${ss}</span>` +
    `<span class="w" title="${esc(l.source)}">${esc(l.who)}</span>` +
    `<span class="g">${esc(l.tag || "-")}</span>` +
    `<span class="m">${esc(l.msg)}</span></div>`;
}

/// Re-render everything held, for a filter change.
///
/// The queue is flushed into it rather than left pending, because the lines in it
/// are already in LIVE_LINES and would otherwise be rendered twice.
function drawLive() {
  const feed = $("live-feed");
  LIVE_QUEUE = [];
  if (LIVE_RAF !== null) {
    cancelAnimationFrame(LIVE_RAF);
    LIVE_RAF = null;
  }
  $("live-empty").hidden = LIVE_ON || LIVE_LINES.length > 0;
  feed.innerHTML = LIVE_LINES.filter(liveFilter()).slice(-LIVE_MAX)
    .map((l) => liveRow(l, false)).join("");
  pinLive(feed);
  liveCount();
}

// Drop the oldest rows, without moving the ones still on screen.
//
// Rows are removed from the top, so shortening the content above the viewport
// while scrollTop stays put slides every visible line up by exactly the height
// removed. Taking that height back out of scrollTop leaves them still. It matters
// as much when the reader is holding the feed: they are parked reading something,
// and without this every trim yanks it upward.
function trimFeed(feed) {
  const over = feed.childElementCount - LIVE_MAX;
  if (over <= 0) return;
  const before = feed.scrollHeight;
  // Collected first: removing from a live HTMLCollection while iterating it skips
  // every other row.
  for (const el of Array.prototype.slice.call(feed.children, 0, over)) {
    el.remove();
  }
  feed.scrollTop -= before - feed.scrollHeight;
}

// The counters and the notes, cheap enough to call on every frame.
function liveCount() {
  const feed = $("live-feed");
  const behind = LIVE_QUEUE.length ? `, ${LIVE_QUEUE.length} arriving` : "";
  $("live-count").textContent = LIVE_LINES.length
    ? `${feed.childElementCount} shown, ${LIVE_LINES.length} held${behind}`
    : "";
  const notes = [];
  if (LIVE_MISSED) {
    notes.push(`${LIVE_MISSED} line(s) arrived faster than this page collected them and are no
      longer in the portal's window. They are still in the per-sender files above; only this
      view lost them.`);
  }
  if (LIVE_SKIPPED) {
    notes.push(`${LIVE_SKIPPED} line(s) were skipped to keep this view current rather than
      letting it fall minutes behind. They are in the files above.`);
  }
  setHTML($("live-missed"), notes.map((t) => `<div class="step">
      <div class="note" style="border-color:var(--warn)">${t}</div>
    </div>`).join(""));
}

// ---------------------------------------------------------------------------
// Who is moving the feed

function liveGesture() {
  LIVE_GESTURE = Date.now();
}

// Following is a position, not a mode: if the reader is at the bottom we follow,
// and if they have scrolled away we do not. Deriving it from where the feed
// actually is means there is no separate state to get out of step with what they
// can see.
function liveScrolled() {
  const feed = $("live-feed");
  // Our own pin, arriving as an ordinary scroll event. Without this the drip
  // would repeatedly be mistaken for the reader scrolling.
  if (Math.abs(feed.scrollTop - LIVE_SET_TO) < 2) return;
  if (Date.now() - LIVE_GESTURE > 1500) return;
  const atBottom = feed.scrollHeight - feed.scrollTop - feed.clientHeight < 24;
  if (atBottom === LIVE_HELD) {
    LIVE_HELD = !atBottom;
    liveState();
  }
}

// Back to following, for a reader who would rather press something than scroll
// several hundred lines.
function followLive() {
  LIVE_HELD = false;
  pinLive($("live-feed"));
  liveState();
}

// What the feed is doing, and when it is holding, how to set it going again.
// Offered as a control rather than an instruction, because "scroll to the bottom"
// is a thing to do and not a thing to read.
function liveState(text) {
  const el = $("live-state");
  if (text) {
    el.textContent = text;
    return;
  }
  if (!LIVE_ON) {
    el.textContent = "stopped";
  } else if (LIVE_HELD) {
    setHTML(el, `held &mdash; <a href="#" onclick="followLive();return false">follow again</a>`);
  } else {
    el.textContent = "following";
  }
}

// Fill the screen with the feed, and come back out.
//
// The native Fullscreen API rather than a fixed-position class, so Escape works,
// the browser's own chrome goes away, and nothing here has to guess at a z-index
// that beats everything else on the page. Where it is unavailable or refused - some
// browsers only grant it inside a user gesture, and an iframe may not have the
// permission at all - the card stays where it is and says so, which is better than
// half-applying a layout nobody can get out of.
async function fullLive() {
  const card = $("live-card");
  try {
    if (document.fullscreenElement) {
      await document.exitFullscreen();
    } else {
      await card.requestFullscreen();
    }
  } catch (e) {
    liveState("this browser would not give the feed the whole screen");
  }
}

// Escape leaves full screen without going through the button, so the label and the
// scroll position are corrected from the event rather than from the click.
document.addEventListener("fullscreenchange", () => {
  const on = document.fullscreenElement === $("live-card");
  $("live-full").textContent = on ? "Leave full screen" : "Full screen";
  // The visible height just changed, so the bottom is somewhere else now.
  const feed = $("live-feed");
  pinLive(feed);
  // Focused on the way in, so PageUp and the arrow keys work against the feed
  // without having to click it first - there is nothing else on screen to click.
  if (on) feed.focus();
});

setInterval(() => { if (!$("app").hidden) refresh(); }, 5000);
</script>
</body>
</html>
"##;
