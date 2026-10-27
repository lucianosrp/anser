//! The browser's side of `finance_core::client`: `fetch()` on the page's
//! own origin with the session cookie (`credentials: same-origin`) and the
//! `X-Finance-Request` header the server wants from a browser on any write;
//! a spawner on the page's event loop; and the file picker for Upload
//! (an `<input type=file>`, its files sent as multipart `files`).

use std::cell::RefCell;

use finance_core::client::{self, BoxFuture, Reply, Request, Response};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

thread_local! {
    /// the files the picker last chose
    static PICKED: RefCell<Option<web_sys::FileList>> = const { RefCell::new(None) };
}

fn err(e: JsValue) -> String {
    e.as_string().unwrap_or_else(|| format!("{e:?}"))
}

async fn send(method: String, path: String, body: Option<JsValue>, content_type: Option<String>) -> Reply {
    let window = web_sys::window().ok_or("no window")?;
    let init = web_sys::RequestInit::new();
    init.set_method(&method);
    init.set_credentials(web_sys::RequestCredentials::SameOrigin);
    let headers = web_sys::Headers::new().map_err(err)?;
    headers.set("X-Finance-Request", "1").map_err(err)?;
    headers.set("Accept", "application/json").map_err(err)?;
    if let Some(ct) = &content_type {
        headers.set("Content-Type", ct).map_err(err)?;
    }
    init.set_headers(&headers);
    if let Some(b) = &body {
        init.set_body(b);
    }
    let req = web_sys::Request::new_with_str_and_init(&path, &init).map_err(err)?;
    let resp = wasm_bindgen_futures::JsFuture::from(window.fetch_with_request(&req)).await.map_err(err)?;
    let resp: web_sys::Response = resp.dyn_into().map_err(|_| "not a Response".to_string())?;
    let buf = wasm_bindgen_futures::JsFuture::from(resp.array_buffer().map_err(err)?).await.map_err(err)?;
    Ok(Response { status: resp.status(), body: js_sys::Uint8Array::new(&buf).to_vec() })
}

fn fetch(req: Request) -> BoxFuture {
    let body = req.body.as_ref().map(|b| JsValue::from(js_sys::Uint8Array::from(&b[..])));
    Box::pin(send(req.method, req.path, body, req.content_type))
}

fn spawn(fut: BoxFuture) {
    wasm_bindgen_futures::spawn_local(async move {
        let _ = fut.await;
    });
}

fn pick(accept: &'static str) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<String>, String>>>> {
    Box::pin(async move {
        let doc = web_sys::window().and_then(|w| w.document()).ok_or("no document")?;
        let input: web_sys::HtmlInputElement = doc.create_element("input").map_err(err)?.dyn_into().map_err(|_| "not an input".to_string())?;
        input.set_type("file");
        input.set_multiple(!accept.starts_with("image/"));
        input.set_accept(accept);
        let (tx, rx) = client::oneshot::<()>();
        let tx = RefCell::new(Some(tx));
        let on_change = Closure::<dyn FnMut()>::new(move || {
            if let Some(t) = tx.borrow_mut().take() {
                t.send(());
            }
        });
        input.set_onchange(Some(on_change.as_ref().unchecked_ref()));
        input.click();
        rx.await;
        drop(on_change);
        let files = input.files().ok_or("no files chosen")?;
        let names = (0..files.length()).filter_map(|i| files.get(i)).map(|f| f.name()).collect();
        PICKED.with(|p| *p.borrow_mut() = Some(files));
        Ok(names)
    })
}

fn send_picked(path: String) -> BoxFuture {
    Box::pin(async move {
        let form = web_sys::FormData::new().map_err(err)?;
        let files = PICKED.with(|p| p.borrow().clone()).ok_or("choose files first")?;
        for i in 0..files.length() {
            if let Some(f) = files.get(i) {
                // the server lowercases nothing: ingest globs lowercase extensions
                let raw = f.name();
                let name = match raw.rfind('.') {
                    Some(i) if i > 0 => format!("{}{}", &raw[..i], raw[i..].to_lowercase()),
                    _ => raw,
                };
                form.append_with_blob_and_filename("files", &f, &name).map_err(err)?;
            }
        }
        // the browser writes the multipart boundary itself: no Content-Type here
        send("POST".into(), path, Some(form.into()), None).await
    })
}

/// A download: the bytes as a Blob behind a temporary object URL, clicked.
fn save(name: &str, bytes: &[u8]) -> Result<(), String> {
    let arr = js_sys::Uint8Array::from(bytes);
    let parts = js_sys::Array::new();
    parts.push(&arr.buffer());
    let blob = web_sys::Blob::new_with_u8_array_sequence(&parts).map_err(err)?;
    let url = web_sys::Url::create_object_url_with_blob(&blob).map_err(err)?;
    let doc = web_sys::window().and_then(|w| w.document()).ok_or("no document")?;
    let a: web_sys::HtmlAnchorElement = doc.create_element("a").map_err(err)?.dyn_into().map_err(|_| "not an anchor".to_string())?;
    a.set_href(&url);
    a.set_download(name);
    a.click();
    let _ = web_sys::Url::revoke_object_url(&url);
    Ok(())
}

pub fn install() {
    client::set_save_hook(save);
    client::set_fetch_hook(fetch);
    client::set_spawn_hook(spawn);
    client::set_file_hooks(pick, send_picked);
}
