//! Serves a Google Doc as JSON parsed from ArchieML: a port of archieml-js's
//! `examples/google_drive.js` to axum and google-drive3.
//!
//! Visiting `/{document-id}` signs in with Google, exports the document as HTML,
//! converts that to ArchieML text (keeping links and list bullets, which a plain-text
//! export loses), and responds with the parsed document. See README.md for setup.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{Json, Redirect};
use axum::routing::get;
use google_drive3::DriveHub;
use google_drive3::hyper_rustls::{HttpsConnector, HttpsConnectorBuilder};
use google_drive3::hyper_util::client::legacy::Client;
use google_drive3::hyper_util::client::legacy::connect::HttpConnector;
use google_drive3::hyper_util::rt::TokioExecutor;
use http_body_util::BodyExt;
use oauth2::basic::BasicClient;
use oauth2::{
    AuthUrl, AuthorizationCode, ClientId, ClientSecret, CsrfToken, EndpointNotSet, EndpointSet,
    PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, Scope, TokenResponse, TokenUrl,
};
use scraper::{ElementRef, Html, Node, Selector};
use serde::Deserialize;

const ADDRESS: &str = "127.0.0.1:3000";
const REDIRECT_URL: &str = "http://127.0.0.1:3000/oauth2callback";
/// The document served at `/`: the example document used by archieml-js.
const DEFAULT_KEY: &str = "1JjYD90DyoaBuRYNxa4_nqrHKkgZf1HrUj30i3rTWX1s";
/// Exporting a document only needs read access.
const SCOPE: &str = "https://www.googleapis.com/auth/drive.readonly";

type OAuthClient =
    BasicClient<EndpointSet, EndpointNotSet, EndpointNotSet, EndpointNotSet, EndpointSet>;
type DriveConnector = HttpsConnector<HttpConnector>;

/// A sign-in in progress, keyed by its CSRF token (the OAuth `state` parameter).
struct PendingSignIn {
    key: String,
    verifier: PkceCodeVerifier,
}

struct AppState {
    oauth: OAuthClient,
    /// For the token exchange. Redirects are disabled, as the oauth2 crate recommends.
    http: reqwest::Client,
    drive: Client<DriveConnector, google_drive3::common::Body>,
    pending: Mutex<HashMap<String, PendingSignIn>>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let env = |name: &str| {
        std::env::var(name).map_err(|_| format!("set {name} to your OAuth client's credentials"))
    };
    let oauth = BasicClient::new(ClientId::new(env("GOOGLE_CLIENT_ID")?))
        .set_client_secret(ClientSecret::new(env("GOOGLE_CLIENT_SECRET")?))
        .set_auth_uri(AuthUrl::new(
            "https://accounts.google.com/o/oauth2/v2/auth".into(),
        )?)
        .set_token_uri(TokenUrl::new("https://oauth2.googleapis.com/token".into())?)
        .set_redirect_uri(RedirectUrl::new(REDIRECT_URL.into())?);
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let connector = HttpsConnectorBuilder::new()
        .with_native_roots()?
        .https_only()
        .enable_http1()
        .enable_http2()
        .build();
    let drive = Client::builder(TokioExecutor::new()).build(connector);

    let state = Arc::new(AppState {
        oauth,
        http,
        drive,
        pending: Mutex::default(),
    });
    let app = Router::new()
        .route(
            "/",
            get(|state| sign_in(state, Path(DEFAULT_KEY.to_owned()))),
        )
        .route("/oauth2callback", get(oauth_callback))
        .route("/{key}", get(sign_in))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(ADDRESS).await?;
    println!("Example app listening at http://{ADDRESS}");
    axum::serve(listener, app).await?;
    Ok(())
}

/// Starts signing in with Google, remembering which document to fetch afterwards.
async fn sign_in(State(state): State<Arc<AppState>>, Path(key): Path<String>) -> Redirect {
    let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
    let (url, csrf) = state
        .oauth
        .authorize_url(CsrfToken::new_random)
        .add_scope(Scope::new(SCOPE.into()))
        .set_pkce_challenge(challenge)
        .url();
    state
        .pending
        .lock()
        .unwrap()
        .insert(csrf.secret().clone(), PendingSignIn { key, verifier });
    Redirect::to(url.as_str())
}

#[derive(Deserialize)]
struct Callback {
    code: String,
    state: String,
}

/// Finishes signing in, then responds with the document parsed as ArchieML.
async fn oauth_callback(
    State(state): State<Arc<AppState>>,
    Query(callback): Query<Callback>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let failed = |status: StatusCode| move |error: String| (status, error);

    let pending = state.pending.lock().unwrap().remove(&callback.state);
    let PendingSignIn { key, verifier } = pending.ok_or_else(|| {
        failed(StatusCode::BAD_REQUEST)("unknown or expired sign-in; start again".into())
    })?;
    let token = state
        .oauth
        .exchange_code(AuthorizationCode::new(callback.code))
        .set_pkce_verifier(verifier)
        .request_async(&state.http)
        .await
        .map_err(|e| failed(StatusCode::BAD_GATEWAY)(format!("token exchange failed: {e}")))?;

    let hub = DriveHub::new(state.drive.clone(), token.access_token().secret().clone());
    let response = hub
        .files()
        .export(&key, "text/html")
        .doit()
        .await
        .map_err(|e| failed(StatusCode::BAD_GATEWAY)(format!("export failed: {e}")))?;
    let html = response
        .into_body()
        .collect()
        .await
        .map_err(|e| failed(StatusCode::BAD_GATEWAY)(format!("reading export failed: {e}")))?
        .to_bytes();

    let text = html_to_archieml(&String::from_utf8_lossy(&html));
    Ok(Json(to_json(&archier::Value::Object(archier::load(&text)))))
}

/// Converts a Google Docs HTML export to ArchieML text: paragraphs and headings become
/// lines, list items become `* ` lines, links are kept as `<a>` tags (with Google's
/// redirect removed), and anything else, such as tables, is dropped along with its
/// content. Smart quotes inside tags are straightened, since Google Docs adds them.
fn html_to_archieml(html: &str) -> String {
    let document = Html::parse_document(html);
    let body = Selector::parse("body").expect("valid selector");
    let mut text = String::new();
    if let Some(body) = document.select(&body).next() {
        push_children(body, &mut text);
    }
    straighten_quotes_in_tags(&text)
}

fn push_children(element: ElementRef<'_>, out: &mut String) {
    for child in element.children() {
        match child.value() {
            Node::Text(text) => out.push_str(text),
            Node::Element(_) => push_element(ElementRef::wrap(child).expect("element"), out),
            _ => {}
        }
    }
}

fn push_element(element: ElementRef<'_>, out: &mut String) {
    match element.value().name() {
        "span" | "ul" | "ol" => push_children(element, out),
        "p" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
            push_children(element, out);
            out.push('\n');
        }
        "li" => {
            out.push_str("* ");
            push_children(element, out);
            out.push('\n');
        }
        "a" => {
            let Some(href) = element.value().attr("href") else {
                return;
            };
            out.push_str("<a href=\"");
            out.push_str(&untracked_url(href));
            out.push_str("\">");
            push_children(element, out);
            out.push_str("</a>");
        }
        _ => {}
    }
}

/// Google Docs wraps links in a redirect, `https://www.google.com/url?q=<url>&...`.
fn untracked_url(href: &str) -> String {
    url::Url::parse(href)
        .ok()
        .and_then(|url| {
            url.query_pairs()
                .find(|(name, _)| name == "q")
                .map(|(_, target)| target.into_owned())
        })
        .unwrap_or_else(|| href.to_owned())
}

/// Replaces curly quotes with straight ones inside every `<...>` that contains no other
/// `<` or `>`.
fn straighten_quotes_in_tags(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some((before, after_open)) = rest.split_once('<') {
        out.push_str(before);
        out.push('<');
        let next = after_open.find(['<', '>']);
        let Some((inside, tail)) = next.and_then(|i| after_open.split_at_checked(i)) else {
            // Nothing closes this `<`, so the rest is plain text.
            rest = after_open;
            break;
        };
        match tail.strip_prefix('>') {
            Some(after_close) => {
                out.extend(inside.chars().map(|c| match c {
                    '“' | '”' => '"',
                    '‘' | '’' => '\'',
                    c => c,
                }));
                out.push('>');
                rest = after_close;
            }
            // Another `<` comes first, so this one starts no tag.
            None => {
                out.push_str(inside);
                rest = tail;
            }
        }
    }
    out.push_str(rest);
    out
}

fn to_json(value: &archier::Value<'_>) -> serde_json::Value {
    match value {
        archier::Value::String(s) => serde_json::Value::String(s.to_string()),
        archier::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(to_json).collect())
        }
        archier::Value::Object(map) => serde_json::Value::Object(
            map.iter()
                .map(|(k, v)| (k.to_owned(), to_json(v)))
                .collect(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape of a Google Docs HTML export.
    const EXPORT: &str = r#"<html><head><meta content="text/html; charset=UTF-8" http-equiv="content-type"><style type="text/css">.c1{font-weight:700}</style></head><body class="doc-content"><p class="c0"><span class="c1">headline: Ships &amp; Sealing Wax</span></p><p><span>byline: </span><span><a href="https://www.google.com/url?q=https://example.com/a%3Fb%3D1&amp;sa=D&amp;ust=1">Walrus</a></span></p><h2><span>[tags]</span></h2><ul class="lst-kix"><li><span>cabbages</span></li><li><span>kings</span></li></ul><p><span>[]</span></p><table><tr><td><p><span>ignored: yes</span></p></td></tr></table><p><span>quote: &lt;a href=“https://example.com”&gt;link&lt;/a&gt; said “hi”</span></p></body></html>"#;

    #[test]
    fn converts_a_google_docs_export() {
        let text = html_to_archieml(EXPORT);
        assert_eq!(
            text,
            "headline: Ships & Sealing Wax\n\
             byline: <a href=\"https://example.com/a?b=1\">Walrus</a>\n\
             [tags]\n\
             * cabbages\n\
             * kings\n\
             []\n\
             quote: <a href=\"https://example.com\">link</a> said “hi”\n"
        );
        let doc = to_json(&archier::Value::Object(archier::load(&text)));
        assert_eq!(
            doc.get("tags"),
            Some(&serde_json::json!(["cabbages", "kings"]))
        );
        assert!(doc.get("ignored").is_none());
    }

    #[test]
    fn straightens_quotes_only_inside_tags() {
        assert_eq!(
            straighten_quotes_in_tags("“a” <b c=“d” e=‘f’> <<g=“h”> i<“j”"),
            "“a” <b c=\"d\" e='f'> <<g=\"h\"> i<“j”"
        );
    }

    #[test]
    fn links_without_redirects_are_kept() {
        assert_eq!(
            untracked_url("https://example.com/?x=1"),
            "https://example.com/?x=1"
        );
        assert_eq!(untracked_url("#heading"), "#heading");
    }
}
