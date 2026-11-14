//! Browser, page/locator engine, authentication and helpers.

pub mod auth;
pub mod browser;
pub mod page;
pub mod utils;

pub use auth::{
    is_logged_in, load_credentials_from_env, login_with_cookie, login_with_credentials,
    wait_for_manual_login, warm_up_browser,
};
pub use browser::{BrowserManager, BrowserOptions, StorageState};
pub use page::{DEFAULT_TIMEOUT, ElementState, Locator, Page, WaitUntil};
pub use utils::{
    click_see_more_buttons, detect_rate_limit, extract_text_safe, handle_modal_close,
    is_page_loaded, retry_async, scroll_to_bottom, scroll_to_half, wait_for_element_smart,
};
