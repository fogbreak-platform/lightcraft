//! Project links (Help menu, About dialog).

/// The app's id in URLs.
pub const APP: &str = "fogbreak-light";
/// Community link.
pub const DISCORD: &str = "https://fogbreak.io";
/// The Fogbreak website.
pub const WEBSITE: &str = "https://fogbreak.io";
/// This app's page on fogbreak.io.
pub const APP_PAGE: &str = "https://fogbreak.io/apps/fogbreak-light";
/// This app's source repository.
pub const GITHUB: &str = "https://github.com/fogbreak-platform/fogbreak-light";

/// (UI command id, menu label, URL) for each link, in Help-menu order.
/// The user documentation (docs/ in the repository).
pub const HELP: &str = "https://github.com/fogbreak-platform/fogbreak-light/tree/main/docs";

pub const LINKS: &[(&str, &str, &str)] = &[
    ("app.help", "Fogbreak Light Help", HELP),
    ("app.discord", "Community…", DISCORD),
    ("app.website", "Fogbreak Light Website", APP_PAGE),
    ("app.github", "Fogbreak Light on GitHub", GITHUB),
    ("app.artcraft", "Fogbreak Website", WEBSITE),
    ("app.feedback", "Send Feedback…", FEEDBACK),
];

/// Where feedback and bug reports go.
pub const FEEDBACK: &str = "https://github.com/fogbreak-platform/fogbreak-light/issues/new";

/// The URL behind a link command id.
pub fn url_of(cmd: &str) -> Option<&'static str> {
    LINKS.iter().find(|(id, _, _)| *id == cmd).map(|(_, _, u)| *u)
}

/// Open `url` in the user's browser (through the host's `open_url` service).
pub fn open(app: &mut crate::LightcraftApp, url: &str) -> Result<serde_json::Value, String> {
    let open = app.services.open_url.as_mut().ok_or("can't open links here")?;
    open(url)?;
    Ok(serde_json::json!({ "url": url }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_are_https_and_name_this_app() {
        for (id, label, url) in LINKS {
            assert!(url.starts_with("https://"), "{id}");
            assert!(!label.is_empty());
            assert_eq!(url_of(id), Some(*url));
        }
        assert!(APP_PAGE.ends_with(&format!("/apps/{APP}")));
        assert!(GITHUB.ends_with(&format!("/fogbreak-platform/{APP}")));
    }
}
