pub mod bash;
pub mod shell_detect;
pub mod powershell;
mod command_safety;
pub use command_safety::is_dangerous_command;
pub mod prefix_suggestions;
