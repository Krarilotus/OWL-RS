use axum::response::Html;

const OPERATOR_UI_HTML: &str = include_str!("operator_ui.html");

pub fn page() -> Html<&'static str> {
    Html(OPERATOR_UI_HTML)
}
