//! 分类配置的应用层校验与保存编排。

use rusqlite::Connection;

use crate::db::tag_facets::{self, FacetSaveInput, TagFacet};
use crate::error::{AppError, AppResult};

/// 在进入事务前校验 IPC 请求的通用业务约束。
pub fn validate_save_input(input: &FacetSaveInput) -> AppResult<()> {
    tag_facets::validate_key(&input.key)?;
    validate_save_fields(input)
}

fn validate_save_fields(input: &FacetSaveInput) -> AppResult<()> {
    if input.display_name.trim().is_empty() {
        return Err(AppError::invalid_arg("显示名不能为空"));
    }
    if !matches!(input.input_mode.as_str(), "ai_and_manual" | "manual_only") {
        return Err(AppError::invalid_arg(
            "input_mode 只允许 ai_and_manual | manual_only",
        ));
    }
    if !matches!(input.selection_mode.as_str(), "single" | "multi") {
        return Err(AppError::invalid_arg(
            "selection_mode 只允许 single | multi",
        ));
    }
    if input.selection_mode == "multi" && input.max_items.is_some_and(|count| count < 1) {
        return Err(AppError::invalid_arg("多选上限必须为正整数或不限"));
    }
    if !matches!(input.applies_to.as_str(), "all" | "image" | "video") {
        return Err(AppError::invalid_arg(
            "applies_to 只允许 all | image | video",
        ));
    }
    if !matches!(input.facet_kind.as_str(), "tag" | "number") {
        return Err(AppError::invalid_arg("facet_kind 只允许 tag | number"));
    }
    if input.facet_kind == "number"
        && (!input.num_step.is_finite()
            || input.num_step <= 0.0
            || !(0..=10).contains(&input.num_decimals)
            || input.num_min.is_some_and(|value| !value.is_finite())
            || input.num_max.is_some_and(|value| !value.is_finite())
            || matches!((input.num_min, input.num_max), (Some(min), Some(max)) if min > max))
    {
        return Err(AppError::invalid_arg("数值范围、精度或步进无效"));
    }
    Ok(())
}

/// 校验后将所有分类字段交给数据库单事务保存。
pub fn save_facet(conn: &Connection, input: &FacetSaveInput) -> AppResult<TagFacet> {
    let mut normalized = input.clone();
    normalized.key = tag_facets::validate_key(&input.key)?;
    validate_save_fields(&normalized)?;
    tag_facets::save_facet(conn, &normalized)
}

#[cfg(test)]
mod tests {
    use super::{save_facet, validate_save_input};
    use crate::db::tag_facets::FacetSaveInput;

    fn input() -> FacetSaveInput {
        FacetSaveInput {
            key: "brand_info".into(),
            display_name: "品牌信息".into(),
            description: "记录画面中可读出的品牌标识".into(),
            input_mode: "ai_and_manual".into(),
            selection_mode: "multi".into(),
            max_items: None,
            applies_to: "all".into(),
            facet_kind: "number".into(),
            num_min: Some(0.0),
            num_max: Some(20.0),
            num_unit: "个".into(),
            num_decimals: 0,
            num_step: 1.0,
        }
    }

    #[test]
    fn accepts_valid_user_defined_facet_input() {
        assert!(validate_save_input(&input()).is_ok());
    }

    #[test]
    fn rejects_invalid_numeric_range_before_database_write() {
        let mut value = input();
        value.num_min = Some(21.0);
        assert!(validate_save_input(&value).is_err());

        value = input();
        value.num_step = f64::NAN;
        assert!(validate_save_input(&value).is_err());
    }

    #[test]
    fn invalid_ipc_facet_request_does_not_write_any_fields() {
        let conn = crate::db::init_memory().unwrap();
        let mut value = input();
        value.display_name = "  ".into();

        assert!(save_facet(&conn, &value).is_err());
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM tag_facets WHERE key = 'brand_info'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }
}
