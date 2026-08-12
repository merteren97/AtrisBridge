# Product shell

AtrisBridge now separates the desktop shell from synchronization business logic. The product layer owns navigation, readable workspace presentation, a progressive-disclosure inspector, activity, and settings; the existing Rust/Tauri synchronization services remain authoritative for operations.
