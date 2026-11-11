# Vendored challenge solver

These files come from [yt-dlp/ejs](https://github.com/yt-dlp/ejs) (Unlicense)
and are used verbatim:

| file | what it is |
|---|---|
| `lib.min.js` | bundles [meriyah](https://github.com/meriyah/meriyah) 6.1.4 (ISC) and [astring](https://github.com/ternao/astring) 1.9.0 (MIT), plus a JS parser/generator pair used to rewrite the player code |
| `core.min.js` | the solver: parses YouTube's `base.js`, finds the function that computes the `n` (throttling) and `sig` (signature) parameters, rewrites it into a standalone function and evaluates it |

They are plain JavaScript with no browser dependencies, so they run inside the
embedded QuickJS engine (`rquickjs`) without anything else installed.

To update, replace the two files with the current release from
<https://github.com/yt-dlp/ejs/releases> and re-run `cargo test`.
