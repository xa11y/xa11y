//! Minimal WebView test application for xa11y integration tests.
//!
//! Renders a simple HTML page with accessible elements (buttons, headings, links)
//! so that xa11y integration tests can verify that web content inside a WebView
//! is visible through the platform accessibility tree.
//!
//! On Linux this uses WebKitGTK via wry/tao, which exposes the DOM to AT-SPI2.

use tao::{
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoop},
    window::WindowBuilder,
};
use wry::WebViewBuilder;

const HTML: &str = r#"
<!DOCTYPE html>
<html lang="en">
<head><meta charset="utf-8"><title>xa11y WebView Test</title></head>
<body>
  <h1>WebView Test Page</h1>
  <button id="webviewBtn">Click Me WebView</button>
  <a href="https://example.com">Example Link</a>
  <input type="text" aria-label="Name Field" value="Alice" />
  <p>Status: ready</p>
</body>
</html>
"#;

fn main() -> wry::Result<()> {
    let event_loop = EventLoop::new();
    let window = WindowBuilder::new()
        .with_title("xa11y-test-webview")
        .build(&event_loop)
        .unwrap();

    let builder = WebViewBuilder::new().with_html(HTML);

    #[cfg(any(
        target_os = "windows",
        target_os = "macos",
        target_os = "ios",
        target_os = "android"
    ))]
    let _webview = builder.build(&window)?;

    #[cfg(not(any(
        target_os = "windows",
        target_os = "macos",
        target_os = "ios",
        target_os = "android"
    )))]
    let _webview = {
        use tao::platform::unix::WindowExtUnix;
        use wry::WebViewBuilderExtUnix;
        let vbox = window.default_vbox().unwrap();
        builder.build_gtk(vbox)?
    };

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;

        if let Event::WindowEvent {
            event: WindowEvent::CloseRequested,
            ..
        } = event
        {
            *control_flow = ControlFlow::Exit;
        }
    });
}
