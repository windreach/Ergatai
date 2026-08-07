//! Skills discovery NAPI bindings.

use napi::bindgen_prelude::*;
use napi_derive::napi;

use super::guard;

/// 扫描本地所有 skills
#[napi]
pub async fn scan_skills() -> Result<Vec<crate::skills::SkillInfo>> {
    guard();
    tokio::task::spawn_blocking(crate::skills::scan_skills)
        .await
        .map_err(|e| Error::from_reason(format!("task panicked: {}", e)))?
}

/// 获取指定 skill 的详细信息
#[napi]
pub async fn get_skill_detail(name: String) -> Result<crate::skills::SkillDetail> {
    guard();
    tokio::task::spawn_blocking(move || crate::skills::get_skill_detail(name))
        .await
        .map_err(|e| Error::from_reason(format!("task panicked: {}", e)))?
}
