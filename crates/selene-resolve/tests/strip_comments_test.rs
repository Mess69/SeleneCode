#![allow(clippy::unwrap_used)]
//! Task 11 — comment/string blanking for the framework extractors.
//!
//! Every Part B extractor is regex-over-source. Without this, a route inside a
//! comment becomes a route in the graph. The TS suite pins the same case
//! ("extractors ignore commented-out routes"); these are its Rust form.
//!
//! The two structural properties (byte length, newline offsets) are not
//! cosmetic: extractors derive a route's LINE from its match offset, and the
//! line feeds the node id. Blanking that shifted offsets would shift every id.

use regex::Regex;
use selene_core::Language;
use selene_resolve::strip_comments_for_regex;

/// One (language, source) pair whose source contains a commented-out route and
/// a string that looks like code.
struct Case {
    lang: Language,
    src: &'static str,
    /// A regex that MUST NOT match after stripping (it is the commented route).
    route_re: &'static str,
}

const CASES: &[Case] = &[
    Case {
        lang: Language::Typescript,
        src: "// router.get('/dead', deadHandler);\n\
              const doc = \"router.get('/in-a-string', h)\";\n\
              router.get('/live', liveHandler);\n\
              /* router.post('/block', blockHandler); */\n",
        route_re: r"router\.(get|post)\s*\(\s*['\x22](/dead|/block)",
    },
    Case {
        lang: Language::Python,
        src: "# @app.route('/dead')\n\
              doc = \"@app.route('/in-a-string')\"\n\
              @app.route('/live')\n\
              def live(): pass\n\
              \"\"\"@app.route('/in-a-docstring')\"\"\"\n",
        route_re: r"@app\.route\(\s*'/dead",
    },
    Case {
        lang: Language::Java,
        src: "// @GetMapping(\"/dead\")\n\
              String doc = \"@GetMapping(\\\"/in-a-string\\\")\";\n\
              @GetMapping(\"/live\")\n\
              /* @PostMapping(\"/block\") */\n",
        route_re: r#"@(Get|Post)Mapping\("(/dead|/block)"#,
    },
    Case {
        lang: Language::Go,
        src: "// r.GET(\"/dead\", deadHandler)\n\
              doc := \"r.GET(\\\"/in-a-string\\\", h)\"\n\
              r.GET(\"/live\", liveHandler)\n",
        route_re: r#"r\.GET\("/dead"#,
    },
    Case {
        lang: Language::Rust,
        src: "// .route(\"/dead\", get(dead))\n\
              let doc = \"//.route(\\\"/in-a-string\\\", get(h))\";\n\
              .route(\"/live\", get(live))\n",
        route_re: r#"\.route\("/dead"#,
    },
    Case {
        lang: Language::Php,
        src: "// Route::get('/dead', [C::class, 'dead']);\n\
              # Route::get('/hash-dead', [C::class, 'x']);\n\
              $doc = \"Route::get('/in-a-string')\";\n\
              Route::get('/live', [C::class, 'live']);\n",
        route_re: r"Route::get\('(/dead|/hash-dead)",
    },
    Case {
        lang: Language::Ruby,
        src: "# get '/dead', to: 'c#dead'\n\
              doc = \"get '/in-a-string'\"\n\
              get '/live', to: 'c#live'\n\
              =begin\n\
              get '/block', to: 'c#block'\n\
              =end\n",
        route_re: r"get '(/dead|/block)'",
    },
    Case {
        lang: Language::CSharp,
        src: "// [HttpGet(\"/dead\")]\n\
              var doc = \"[HttpGet(\\\"/in-a-string\\\")]\";\n\
              [HttpGet(\"/live\")]\n\
              /* [HttpPost(\"/block\")] */\n",
        route_re: r#"\[Http(Get|Post)\("(/dead|/block)"#,
    },
];

/// The structural contract, for every language: byte length identical, every
/// newline at the same byte offset. Ids depend on it.
#[test]
fn blanking_preserves_byte_length_and_newline_offsets() {
    for case in CASES {
        let out = strip_comments_for_regex(case.src, case.lang);

        assert_eq!(
            out.len(),
            case.src.len(),
            "{:?}: byte length must not change — a shorter output shifts every \
             subsequent match offset, and offsets become route LINES, which become node IDS",
            case.lang
        );

        let nl_in: Vec<usize> = case
            .src
            .bytes()
            .enumerate()
            .filter(|(_, b)| *b == b'\n')
            .map(|(i, _)| i)
            .collect();
        let nl_out: Vec<usize> = out
            .bytes()
            .enumerate()
            .filter(|(_, b)| *b == b'\n')
            .map(|(i, _)| i)
            .collect();
        assert_eq!(
            nl_in, nl_out,
            "{:?}: every newline must stay at its exact byte offset",
            case.lang
        );
    }
}

/// The behavioral contract, for every language: a commented-out route is
/// INVISIBLE to a route regex afterwards, and the live route on the next line
/// survives **with its path intact**.
///
/// The surviving path is half the contract, not a nicety: every Part B extractor
/// captures its path with `['"]([^'"]+)['"]` over this function's output. Blank
/// the string bodies and every route in the graph gets an empty path.
#[test]
fn commented_routes_vanish_and_live_routes_keep_their_paths() {
    for case in CASES {
        let out = strip_comments_for_regex(case.src, case.lang);
        let re = Regex::new(case.route_re).unwrap();

        assert!(
            re.is_match(case.src),
            "{:?}: the fixture must actually contain the dead route BEFORE stripping \
             (else this test proves nothing)",
            case.lang
        );
        assert!(
            !re.is_match(&out),
            "{:?}: a commented-out route must not match after stripping.\n\
             --- stripped ---\n{out}",
            case.lang
        );
        assert!(
            out.contains("/live"),
            "{:?}: the LIVE route must keep its path — strings are SKIPPED, not \
             blanked, precisely so extractors can still read it.\n--- stripped ---\n{out}",
            case.lang
        );
    }
}

/// String-awareness, the reason strings are scanned at all: a `//` (or `#`)
/// INSIDE a string literal must not start a comment and blank the rest of the
/// line. A URL in a config line is the everyday case.
#[test]
fn a_comment_marker_inside_a_string_does_not_start_a_comment() {
    let src = "const url = \"http://example.com/x\";\nrouter.get('/live', h);\n";
    let out = strip_comments_for_regex(src, Language::Typescript);

    assert_eq!(
        out, src,
        "nothing here is a comment — the source is untouched"
    );

    // And the Python/PHP `#` form.
    let src = "url = 'http://x/#frag'\n@app.route('/live')\n";
    let out = strip_comments_for_regex(src, Language::Python);
    assert!(
        out.contains("@app.route('/live')"),
        "a '#' inside a string must not swallow the next line: {out:?}"
    );
}

/// A multi-byte char inside a comment becomes N space BYTES, one per byte —
/// never one space per char (that would shorten the text and move every id
/// after it). This is the rule Phase 2's `pre_parse` already follows.
#[test]
fn multibyte_comment_bodies_blank_bytewise() {
    let src = "// héllo → wörld\ncode();\n";
    let out = strip_comments_for_regex(src, Language::Typescript);

    assert_eq!(
        out.len(),
        src.len(),
        "byte length preserved across non-ASCII"
    );
    assert!(out.starts_with("//"), "the comment opener is kept");
    assert!(
        !out.contains("héllo") && !out.contains('→'),
        "the comment body is gone"
    );
    assert!(out.contains("code();"), "the code after it survives");
    assert!(
        out.lines().next().unwrap().trim_end() == "//",
        "the whole comment body blanked to spaces: {:?}",
        out.lines().next().unwrap()
    );
}

/// Escapes inside strings: a `\"` must not end the string early. If it did, the
/// scanner would think it was back in code, and a later `//` inside the *same*
/// literal would blank real code after it.
#[test]
fn escaped_quotes_do_not_end_a_string_early() {
    let src = "const s = \"a\\\" // not a comment\";\nrouter.get('/live', h);\n";
    let out = strip_comments_for_regex(src, Language::Typescript);

    assert_eq!(out.len(), src.len());
    assert_eq!(
        out, src,
        "the `//` is inside the string (the \\\" did not end it), so nothing is a \
         comment and nothing is blanked"
    );
    assert!(
        out.contains("router.get('/live'"),
        "the live route is intact"
    );
}

/// An unterminated comment (a truncated file) blanks to EOF and does not panic.
/// Extraction is best-effort; errors are collected, never thrown.
#[test]
fn unterminated_constructs_do_not_panic() {
    for (src, lang) in [
        ("/* never closed\nrouter.get('/x', h)", Language::Typescript),
        ("const s = \"never closed\n", Language::Typescript),
        ("\"\"\"never closed\n", Language::Python),
    ] {
        let out = strip_comments_for_regex(src, lang);
        assert_eq!(out.len(), src.len(), "byte length preserved even truncated");
    }
}

/// A language with no syntax entry is returned untouched — the safe default. A
/// wrong guess would corrupt the source an extractor is about to scan.
#[test]
fn an_unknown_language_is_returned_unchanged() {
    let src = "-- a SQL comment\nSELECT 1;\n";
    assert_eq!(strip_comments_for_regex(src, Language::Yaml), src);
}
