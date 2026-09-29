//! 发布素材的幂等导入：把工程师写的素材文件（`config/bootstrap/*.json` 这类）写成
//! `catalog.vendor_models`、`supply.channels`、`supply.offerings` 与 `pricing.price_plans`。
//!
//! 它是**工程侧**的动作：随服务启动、在迁移之后跑一次，素材目录由 [`SUPPLY_MATERIAL_DIR_ENV`] 给；
//! 不新增运营入口，也不新增工程端点。素材写"应该有什么"，库记录"实际是什么"。各张表的匹配键、
//! 命中时更新什么、以及 `base_url` / `credential_env` 变了为什么不就地改名，见
//! `docs/design/0012-platform-model-publishing.md` §3。
//!
//! 幂等靠**既有的身份键**，不靠"按名字找"：同一份素材跑两次，四张表都不产生第二行。
//!
//! 素材里的 `consumer_rates_cny` / `reference_cost_microusd` / `cost_basis` / `floor_amounts`
//! 与顶层 `markup_bps` **一律不导入**：那是**运营的定价**，随发布物按候选给、落在
//! `publication.runtime_revisions` 的定价列上，不属于这四张表。导入它们等于让一份工程素材顶掉
//! 运营的价。`_comment` / `_status` / `_evidence` 是说明字段；顶层 `gateway_model` 也不在这里落地
//! ——对客名由运营发布时自己填，导入不建立 Gateway Model。
//!
//! 素材里的 `cost_currency` 同样不落这四张表：`supply.offerings` 没有这一列，成本币种是发布期
//! 按候选声明的东西（今天由 Price Plan 的币种或发布命令的 `cost_currency` 承接）。

use crate::{database_error, to_i64};
use seeai_application::ApplicationError;
use seeai_domain::{ChannelId, OfferingId, PricingFormula, VendorModelId};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool, Row};
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// 素材目录的环境变量名。
///
/// **不设时默认 [`DEFAULT_SUPPLY_MATERIAL_DIR`]**：工程师写的素材就是供给的来源（随仓库与镜像
/// 一起发布），运营上架模型不该依赖谁记着去开一个环境变量。设成**空白**＝显式不导入（测试库与
/// 开发库要一份干净的供给清单时用它）；目录不存在、目录里没有 `*.json` 时什么都不做。
pub const SUPPLY_MATERIAL_DIR_ENV: &str = "SUPPLY_MATERIAL_DIR";

/// 没设 `SUPPLY_MATERIAL_DIR` 时用的目录（相对进程工作目录）。
///
/// 仓库、systemd 部署（`WorkingDirectory=/opt/seeai`）与 Docker 镜像（`WORKDIR /app`）都把
/// `config/bootstrap` 放在工作目录下，所以这一个默认值在三种部署里都指得到素材。
pub const DEFAULT_SUPPLY_MATERIAL_DIR: &str = "config/bootstrap";

/// 素材没写 `actor` 时，价目表那行的来源标注。
const DEFAULT_ACTOR: &str = "supply-material-import";

/// 一次导入做了什么，供启动日志读出"这次到底导了多少"。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MaterialImportSummary {
    /// 读进去的素材文件数。
    pub materials: usize,
    /// 见过的供给条数（新建与更新都算）。
    pub offerings: usize,
    /// **新建**的价目表行数（命中既有行、复用它时不计）。
    pub price_plans: usize,
}

/// 按环境变量给的目录导入素材；目录不存在时什么都不做。
///
/// 素材存在但读不出来、或者形状不对，是**错误**而不是跳过：那是工程输入写错了，静默跳过会让库
/// 与素材长期不一致，而那种不一致只会在发布或受理时才显形。调用方让启动失败即可。
pub async fn import_supply_materials_from_env(
    pool: &PgPool,
) -> Result<MaterialImportSummary, ApplicationError> {
    let Some(dir) = material_dir_from(std::env::var(SUPPLY_MATERIAL_DIR_ENV).ok().as_deref())
    else {
        return Ok(MaterialImportSummary::default());
    };
    import_supply_materials(pool, &dir).await
}

/// 从环境变量的取值决定素材目录。
///
/// `None`（没设）＝默认目录；空白（显式设成空）＝不导入；其余＝该路径。
fn material_dir_from(value: Option<&str>) -> Option<PathBuf> {
    match value {
        Some(raw) if raw.trim().is_empty() => None,
        Some(raw) => Some(PathBuf::from(raw.trim())),
        None => Some(PathBuf::from(DEFAULT_SUPPLY_MATERIAL_DIR)),
    }
}

/// 把 `dir` 下的素材逐个导入。每份素材一个事务：一份写坏不影响别的，也不留半份进去的状态。
pub async fn import_supply_materials(
    pool: &PgPool,
    dir: &Path,
) -> Result<MaterialImportSummary, ApplicationError> {
    let materials = read_materials(dir)?;
    let mut summary = MaterialImportSummary::default();
    for (path, material) in &materials {
        let one = import_material(pool, path, material).await?;
        summary.materials += 1;
        summary.offerings += one.offerings;
        summary.price_plans += one.price_plans;
    }
    Ok(summary)
}

/// 读目录里的素材。路径排序后处理：两份素材撞上同一个身份键时，"谁最后写"是确定的。
fn read_materials(dir: &Path) -> Result<Vec<(PathBuf, Material)>, ApplicationError> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let entries = std::fs::read_dir(dir).map_err(|error| {
        ApplicationError::Validation(format!("cannot read {}: {error}", dir.display()))
    })?;
    let mut paths = Vec::new();
    for entry in entries {
        let path = entry
            .map_err(|error| {
                ApplicationError::Validation(format!("cannot walk {}: {error}", dir.display()))
            })?
            .path();
        if path.extension().and_then(std::ffi::OsStr::to_str) == Some("json") {
            paths.push(path);
        }
    }
    paths.sort();
    let mut materials = Vec::with_capacity(paths.len());
    for path in paths {
        let text = std::fs::read_to_string(&path).map_err(|error| {
            ApplicationError::Validation(format!("cannot read {}: {error}", path.display()))
        })?;
        let material: Material = serde_json::from_str(&text).map_err(|error| {
            ApplicationError::Validation(format!("{}: {error}", path.display()))
        })?;
        materials.push((path, material));
    }
    Ok(materials)
}

async fn import_material(
    pool: &PgPool,
    path: &Path,
    material: &Material,
) -> Result<MaterialImportSummary, ApplicationError> {
    let mut tx = pool.begin().await.map_err(database_error)?;
    let vendor_model_id = upsert_vendor_model(&mut tx, path, material).await?;
    let mut summary = MaterialImportSummary::default();
    for (index, offering) in material.offerings.iter().enumerate() {
        let label = format!("{}: offerings[{index}]", path.display());
        check_offering_shape(&label, offering)?;
        let channel_id = upsert_channel(&mut tx, offering).await?;
        let offering_id = upsert_offering(&mut tx, vendor_model_id, channel_id, offering).await?;
        summary.offerings += 1;
        if let Some(plan) = offering.price_plan.as_ref()
            && upsert_price_plan(&mut tx, offering_id, plan, material.actor.as_deref()).await?
        {
            summary.price_plans += 1;
        }
    }
    tx.commit().await.map_err(database_error)?;
    Ok(summary)
}

/// 合同行按 `(vendor_id, native_model_id, native_revision)` 复用，**只插不改**。
///
/// 素材没有 `schema_hash` 这一维，库层也没有这一列：`0006` 把"同一修订按合同内容分叉成多行"这件事
/// 取消了，合同是模型级唯一一份。因此同一个修订下**内容不同**（合同被改过）不是"新一行"，而是
/// 素材自己写错了修订标识——报错点名，由工程师改 `native_revision`。合同行不可变是"Job 固定受理时
/// 版本"成立的前提，导入不能悄悄改写它。
async fn upsert_vendor_model(
    conn: &mut PgConnection,
    path: &Path,
    material: &Material,
) -> Result<VendorModelId, ApplicationError> {
    let inserted: Option<Uuid> = sqlx::query_scalar(
        r#"
        INSERT INTO catalog.vendor_models
            (id, vendor_id, native_model_id, native_revision, capability_schema)
        VALUES ($1, $2, $3, $4, $5)
        ON CONFLICT (vendor_id, native_model_id, native_revision) DO NOTHING
        RETURNING id
        "#,
    )
    .bind(VendorModelId::new().0)
    .bind(&material.vendor_id)
    .bind(&material.native_model_id)
    .bind(&material.native_revision)
    .bind(&material.capability_schema)
    .fetch_optional(&mut *conn)
    .await
    .map_err(database_error)?;
    if let Some(id) = inserted {
        return Ok(VendorModelId(id));
    }
    let existing = sqlx::query(
        r#"
        SELECT id, capability_schema FROM catalog.vendor_models
        WHERE vendor_id = $1 AND native_model_id = $2 AND native_revision = $3
        "#,
    )
    .bind(&material.vendor_id)
    .bind(&material.native_model_id)
    .bind(&material.native_revision)
    .fetch_one(&mut *conn)
    .await
    .map_err(database_error)?;
    let stored: Value = existing
        .try_get("capability_schema")
        .map_err(database_error)?;
    if stored != material.capability_schema {
        return Err(ApplicationError::Validation(format!(
            "{}: vendor model {} revision {} is already in the catalog with a different contract; \
             the contract row is immutable, so publish the change under a new native_revision",
            path.display(),
            material.native_model_id,
            material.native_revision
        )));
    }
    Ok(VendorModelId(
        existing.try_get("id").map_err(database_error)?,
    ))
}

/// 渠道按身份三要素 `(provider_kind, base_url, credential_env)` 复用，命中时**什么都不更新**。
///
/// `enabled` 是运营设的停用状态：写回 `true` 会把手工停用无声顶掉，所以这里连 `DO UPDATE` 都不做
/// ——渠道除了身份与这个开关没有可变量。地址或凭证变量名变了就是**另一个渠道身份**，落新行，旧行
/// 留着（它可能还被别的 Offering 或已发布修订引用）；导入绝不就地改名。
async fn upsert_channel(
    conn: &mut PgConnection,
    offering: &MaterialOffering,
) -> Result<ChannelId, ApplicationError> {
    let inserted: Option<Uuid> = sqlx::query_scalar(
        r#"
        INSERT INTO supply.channels
            (id, provider_kind, base_url, credential_env, enabled)
        VALUES ($1, $2, $3, $4, true)
        ON CONFLICT (provider_kind, base_url, credential_env) DO NOTHING
        RETURNING id
        "#,
    )
    .bind(ChannelId::new().0)
    .bind(&offering.provider_kind)
    .bind(&offering.base_url)
    .bind(&offering.credential_env)
    .fetch_optional(&mut *conn)
    .await
    .map_err(database_error)?;
    if let Some(id) = inserted {
        return Ok(ChannelId(id));
    }
    let id: Uuid = sqlx::query_scalar(
        r#"
        SELECT id FROM supply.channels
        WHERE provider_kind = $1 AND base_url = $2 AND credential_env = $3
        "#,
    )
    .bind(&offering.provider_kind)
    .bind(&offering.base_url)
    .bind(&offering.credential_env)
    .fetch_one(&mut *conn)
    .await
    .map_err(database_error)?;
    Ok(ChannelId(id))
}

/// 供给按 `(vendor_model_id, channel_id)` 复用，命中时更新**技术定义**，`enabled` 不在更新之列。
///
/// 技术定义属于工程师（素材是它的来源），所以素材改了就地更新；`enabled` 属于运营（停用一条供给
/// 要立刻对之后的受理生效），所以重导一次不该把它顶回启用。这一组列与 `publish_runtime` 更新的是
/// 同一组——导入与发布引用同一条供给，两边对"什么是可变量"必须是同一个答案。
async fn upsert_offering(
    conn: &mut PgConnection,
    vendor_model_id: VendorModelId,
    channel_id: ChannelId,
    offering: &MaterialOffering,
) -> Result<OfferingId, ApplicationError> {
    let id: Uuid = sqlx::query_scalar(
        r#"
        INSERT INTO supply.offerings
            (id, vendor_model_id, channel_id, adapter_key, provider_model_id,
             restrictions, carrier_schema, parameter_mapping, enabled,
             formula, cost_unit_price_microusd)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, true, $9, $10)
        ON CONFLICT (vendor_model_id, channel_id) DO UPDATE SET
            adapter_key = EXCLUDED.adapter_key,
            provider_model_id = EXCLUDED.provider_model_id,
            restrictions = EXCLUDED.restrictions,
            carrier_schema = EXCLUDED.carrier_schema,
            parameter_mapping = EXCLUDED.parameter_mapping,
            formula = EXCLUDED.formula,
            cost_unit_price_microusd = EXCLUDED.cost_unit_price_microusd
        RETURNING id
        "#,
    )
    .bind(OfferingId::new().0)
    .bind(vendor_model_id.0)
    .bind(channel_id.0)
    .bind(&offering.adapter_key)
    .bind(&offering.provider_model_id)
    .bind(&offering.restrictions)
    .bind(&offering.carrier_schema)
    .bind(&offering.parameter_mapping)
    .bind(offering.formula.as_str())
    .bind(offering.cost_unit_price_microusd.map(to_i64).transpose()?)
    .fetch_one(&mut *conn)
    .await
    .map_err(database_error)?;
    Ok(OfferingId(id))
}

/// 价目表按**内容**复用：`(offering_id, currency, source_url)` 与四档费率全同的那一行存在就复用它，
/// 否则追加一行。返回"是否新建"。
///
/// 为什么不是"只按 `(currency, source_url)` 命中就复用"：费率变了必须留下新的一行——价目表是
/// 带写入时刻的追加形态，就地改费率会让"这条供给当时的成本是多少"没法重建，与
/// `pricing.fx_rates` 的形态也不一致。出处或费率任一变化都落新行，两者都没变就复用，于是同一份
/// 素材跑两次不产生第二行。
async fn upsert_price_plan(
    conn: &mut PgConnection,
    offering_id: OfferingId,
    plan: &MaterialPricePlan,
    actor: Option<&str>,
) -> Result<bool, ApplicationError> {
    let existing: Option<Uuid> = sqlx::query_scalar(
        r#"
        SELECT id FROM pricing.price_plans
        WHERE offering_id = $1 AND currency = $2 AND source_url = $3
          AND text_input_microusd_per_million = $4
          AND image_input_microusd_per_million = $5
          AND text_output_microusd_per_million = $6
          AND image_output_microusd_per_million = $7
        LIMIT 1
        "#,
    )
    .bind(offering_id.0)
    .bind(&plan.currency)
    .bind(&plan.source_url)
    .bind(to_i64(plan.text_input_microusd_per_million)?)
    .bind(to_i64(plan.image_input_microusd_per_million)?)
    .bind(to_i64(plan.text_output_microusd_per_million)?)
    .bind(to_i64(plan.image_output_microusd_per_million)?)
    .fetch_optional(&mut *conn)
    .await
    .map_err(database_error)?;
    if existing.is_some() {
        return Ok(false);
    }
    sqlx::query(
        r#"
        INSERT INTO pricing.price_plans (
            id, offering_id, currency,
            text_input_microusd_per_million, image_input_microusd_per_million,
            text_output_microusd_per_million, image_output_microusd_per_million,
            source_url, approved_by
        ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)
        "#,
    )
    .bind(Uuid::new_v4())
    .bind(offering_id.0)
    .bind(&plan.currency)
    .bind(to_i64(plan.text_input_microusd_per_million)?)
    .bind(to_i64(plan.image_input_microusd_per_million)?)
    .bind(to_i64(plan.text_output_microusd_per_million)?)
    .bind(to_i64(plan.image_output_microusd_per_million)?)
    .bind(&plan.source_url)
    .bind(actor.unwrap_or(DEFAULT_ACTOR))
    .execute(&mut *conn)
    .await
    .map_err(database_error)?;
    Ok(true)
}

/// 素材内部的配套校验：计价形态、它的参数、以及这条供给要不要一份价目表。
///
/// 库层只钉得住"单价只属于按张 / 按次"（`offerings_cost_unit_price_shape`）；"`token_rates` 必须有
/// 四档费率、另外三种形态没有价目表"是发布期的判据。导入期在这里拦一道，是为了让错误说到**哪个
/// 文件的哪条候选**，而不是等发布时才在别处炸开。
fn check_offering_shape(label: &str, offering: &MaterialOffering) -> Result<(), ApplicationError> {
    let formula = offering.formula.as_str();
    match (
        offering.formula.takes_unit_price(),
        offering.cost_unit_price_microusd,
    ) {
        (true, None) => {
            return Err(ApplicationError::Validation(format!(
                "{label}: {formula} needs cost_unit_price_microusd"
            )));
        }
        (false, Some(_)) => {
            return Err(ApplicationError::Validation(format!(
                "{label}: {formula} takes no cost_unit_price_microusd"
            )));
        }
        _ => {}
    }
    match (
        offering.formula == PricingFormula::TokenRates,
        offering.price_plan.is_some(),
    ) {
        (true, false) => Err(ApplicationError::Validation(format!(
            "{label}: {formula} needs the four-tier price_plan"
        ))),
        (false, true) => Err(ApplicationError::Validation(format!(
            "{label}: {formula} must not carry a price plan; the four-tier rates belong to \
             token_rates only"
        ))),
        _ => Ok(()),
    }
}

/// 一份素材。顶层那些说明字段与运营的定价字段不在结构里，serde 默认忽略未知字段，这正好是我们要的：
/// 素材写"应该有什么"，但**哪些字段属于导入**由这个结构说了算。
#[derive(Debug, Deserialize)]
struct Material {
    vendor_id: String,
    native_model_id: String,
    native_revision: String,
    capability_schema: Value,
    /// 谁写的这份素材；落进价目表那行的来源标注。
    actor: Option<String>,
    offerings: Vec<MaterialOffering>,
}

#[derive(Debug, Deserialize)]
struct MaterialOffering {
    provider_kind: String,
    adapter_key: String,
    provider_model_id: String,
    base_url: String,
    credential_env: String,
    #[serde(default = "empty_object")]
    restrictions: Value,
    carrier_schema: Value,
    #[serde(default = "empty_object")]
    parameter_mapping: Value,
    formula: PricingFormula,
    /// 按张 / 按次计费的单价；另外两种形态必须不写。
    cost_unit_price_microusd: Option<u64>,
    /// 按 token 计量量计价时的那份四档费率；另外三种形态必须不写。
    price_plan: Option<MaterialPricePlan>,
}

#[derive(Debug, Deserialize)]
struct MaterialPricePlan {
    currency: String,
    text_input_microusd_per_million: u64,
    image_input_microusd_per_million: u64,
    text_output_microusd_per_million: u64,
    image_output_microusd_per_million: u64,
    #[serde(default)]
    source_url: String,
}

fn empty_object() -> Value {
    json!({})
}

#[cfg(test)]
mod tests;
