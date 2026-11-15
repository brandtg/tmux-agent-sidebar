mod button;
mod prompts;
mod section;

pub(in crate::ui) use button::has_info;
pub(crate) use prompts::prompt_for_agent;
pub(in crate::ui) use section::{materialize_copy_targets, section_lines, section_metrics};
