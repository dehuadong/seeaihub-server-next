use base64::{Engine, engine::general_purpose::STANDARD};
use reqwest::{Client, StatusCode};
use seeai_application::HubRepository;
use seeai_persistence::PgHubRepository;
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
use std::{
    net::TcpListener,
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::Duration,
};
use uuid::Uuid;

struct ApiProcess {
    child: Child,
    asset_root: PathBuf,
}

impl Drop for ApiProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.asset_root);
    }
}

#[tokio::test]
#[ignore = "requires an empty PostgreSQL database via HTTP_CONTRACT_DATABASE_URL"]
async fn image_generation_http_contract() {
    let database_url = std::env::var("HTTP_CONTRACT_DATABASE_URL")
        .expect("HTTP_CONTRACT_DATABASE_URL is required for the ignored contract test");
    let listener = TcpListener::bind("127.0.0.1:0").expect("test port should bind");
    let port = listener.local_addr().expect("test address").port();
    drop(listener);
    let base_url = format!("http://127.0.0.1:{port}");
    let admin_token = format!("contract-admin-{}", Uuid::new_v4());
    let asset_root = std::env::temp_dir().join(format!("seeai-contract-{}", Uuid::new_v4()));
    let child = Command::new(env!("CARGO_BIN_EXE_seeai-api"))
        .env("DATABASE_URL", &database_url)
        .env("API_BIND", format!("127.0.0.1:{port}"))
        .env("ADMIN_TOKEN", &admin_token)
        .env("ASSET_STORE", "local")
        .env("ASSET_LOCAL_ROOT", &asset_root)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("API process should start");
    let _process = ApiProcess { child, asset_root };
    let client = Client::new();
    wait_until_ready(&client, &base_url).await;

    let unauthorized = client
        .post(format!("{base_url}/admin/accounts"))
        .json(&json!({"initial_credit_microusd": 1}))
        .send()
        .await
        .expect("unauthorized request");
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let account = create_account(&client, &base_url, &admin_token).await;
    let api_key = issue_key(&client, &base_url, &admin_token, &account).await;
    publish_bootstrap(&client, &base_url, &admin_token).await;
    reject_mismatched_model_identity(&client, &base_url, &admin_token).await;

    let png = STANDARD
        .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M/wHwAF/gL+XcY4WQAAAABJRU5ErkJggg==")
        .expect("PNG fixture");
    let asset_response = client
        .post(format!("{base_url}/v1/assets"))
        .bearer_auth(&api_key)
        .header("content-type", "image/png")
        .header("x-asset-role", "image")
        .body(png)
        .send()
        .await
        .expect("asset upload");
    assert_eq!(asset_response.status(), StatusCode::CREATED);
    let asset: Value = asset_response.json().await.expect("asset JSON");
    assert_eq!(asset["width"], 1);
    assert_eq!(asset["height"], 1);

    let request_key = format!("contract-{}", Uuid::new_v4());
    let request = generation_request(&request_key, "contract prompt");
    let first = client
        .post(format!("{base_url}/v1/image-generations"))
        .bearer_auth(&api_key)
        .json(&request)
        .send()
        .await
        .expect("first generation");
    assert_eq!(first.status(), StatusCode::ACCEPTED);
    let first: Value = first.json().await.expect("first job JSON");
    let repeated: Value = client
        .post(format!("{base_url}/v1/image-generations"))
        .bearer_auth(&api_key)
        .json(&request)
        .send()
        .await
        .expect("repeated generation")
        .json()
        .await
        .expect("repeated job JSON");
    assert_eq!(first["job_id"], repeated["job_id"]);
    assert_eq!(
        first.as_object().expect("response object").len(),
        4,
        "creation response must not leak runtime/provider fields"
    );

    let conflict = client
        .post(format!("{base_url}/v1/image-generations"))
        .bearer_auth(&api_key)
        .json(&generation_request(&request_key, "changed prompt"))
        .send()
        .await
        .expect("conflicting generation");
    assert_eq!(conflict.status(), StatusCode::CONFLICT);
    let job_id = first["job_id"].as_str().expect("job id");
    let job = client
        .get(format!("{base_url}/v1/image-generations/{job_id}"))
        .bearer_auth(&api_key)
        .send()
        .await
        .expect("job query");
    assert_eq!(job.status(), StatusCode::OK);

    let other_account = create_account(&client, &base_url, &admin_token).await;
    let other_key = issue_key(&client, &base_url, &admin_token, &other_account).await;
    let foreign_asset = client
        .get(format!(
            "{base_url}/v1/assets/{}",
            asset["id"].as_str().expect("asset id")
        ))
        .bearer_auth(other_key)
        .send()
        .await
        .expect("foreign asset query");
    assert_eq!(foreign_asset.status(), StatusCode::NOT_FOUND);

    verify_reconciliation_contract(&client, &base_url, &admin_token, &api_key, &database_url).await;
    verify_lease_recovery_contract(&client, &base_url, &api_key, &database_url).await;
}

async fn wait_until_ready(client: &Client, base_url: &str) {
    for _ in 0..100 {
        if client
            .get(format!("{base_url}/health"))
            .send()
            .await
            .is_ok_and(|response| response.status().is_success())
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("API did not become ready");
}

async fn create_account(client: &Client, base_url: &str, admin_token: &str) -> String {
    let response = client
        .post(format!("{base_url}/admin/accounts"))
        .bearer_auth(admin_token)
        .json(&json!({"initial_credit_microusd": 100_000}))
        .send()
        .await
        .expect("account creation");
    assert_eq!(response.status(), StatusCode::OK);
    response.json::<Value>().await.expect("account JSON")["account_id"]
        .as_str()
        .expect("account id")
        .to_owned()
}

async fn issue_key(client: &Client, base_url: &str, admin_token: &str, account_id: &str) -> String {
    client
        .post(format!("{base_url}/admin/accounts/{account_id}/api-keys"))
        .bearer_auth(admin_token)
        .json(&json!({"label": "contract"}))
        .send()
        .await
        .expect("key creation")
        .json::<Value>()
        .await
        .expect("key JSON")["api_key"]
        .as_str()
        .expect("API key")
        .to_owned()
}

async fn publish_bootstrap(client: &Client, base_url: &str, admin_token: &str) {
    let config: Value = serde_json::from_str(include_str!(
        "../../../config/bootstrap/aihubmix-gpt-image-2.json"
    ))
    .expect("bootstrap config");
    let response = client
        .post(format!("{base_url}/admin/runtime-revisions"))
        .bearer_auth(admin_token)
        .json(&config)
        .send()
        .await
        .expect("runtime publication");
    assert_eq!(response.status(), StatusCode::OK);
}

async fn reject_mismatched_model_identity(client: &Client, base_url: &str, admin_token: &str) {
    let mut config: Value = serde_json::from_str(include_str!(
        "../../../config/bootstrap/aihubmix-gpt-image-2.json"
    ))
    .expect("bootstrap config");
    config["native_model_id"] = Value::String("different-model".to_owned());
    let response = client
        .post(format!("{base_url}/admin/runtime-revisions"))
        .bearer_auth(admin_token)
        .json(&config)
        .send()
        .await
        .expect("mismatched publication");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

fn generation_request(idempotency_key: &str, prompt: &str) -> Value {
    json!({
        "native_model_id": "gpt-image-2",
        "native_parameters": {"prompt": prompt, "n": 1, "extra": {"quality": "low"}},
        "asset_bindings": [],
        "idempotency_key": idempotency_key,
        "max_cost_microusd": 20_000
    })
}

async fn verify_reconciliation_contract(
    client: &Client,
    base_url: &str,
    admin_token: &str,
    api_key: &str,
    database_url: &str,
) {
    let key = format!("reconciliation-{}", Uuid::new_v4());
    let job: Value = client
        .post(format!("{base_url}/v1/image-generations"))
        .bearer_auth(api_key)
        .json(&generation_request(&key, "reconciliation contract"))
        .send()
        .await
        .expect("reconciliation job")
        .json()
        .await
        .expect("reconciliation job JSON");
    let job_id = Uuid::parse_str(job["job_id"].as_str().expect("job id")).expect("job UUID");
    let attempt_id = Uuid::new_v4();
    let case_id = Uuid::new_v4();
    let pool = PgPool::connect(database_url)
        .await
        .expect("contract database");
    let balance_after_hold: i64 = sqlx::query_scalar(
        "SELECT a.balance_microusd FROM ledger.accounts a JOIN generation.jobs j ON j.account_id = a.id WHERE j.id = $1",
    )
    .bind(job_id)
    .fetch_one(&pool)
    .await
    .expect("balance after hold");
    sqlx::query("UPDATE generation.jobs SET state = 'reconciliation_required' WHERE id = $1")
        .bind(job_id)
        .execute(&pool)
        .await
        .expect("job reconciliation state");
    sqlx::query(
        "INSERT INTO generation.attempts (id, job_id, state, request_digest) VALUES ($1,$2,'reconciliation_required','contract')",
    )
    .bind(attempt_id)
    .bind(job_id)
    .execute(&pool)
    .await
    .expect("attempt fixture");
    sqlx::query(
        "INSERT INTO operations.reconciliation_cases (id, job_id, attempt_id, reason) VALUES ($1,$2,$3,'contract')",
    )
    .bind(case_id)
    .bind(job_id)
    .bind(attempt_id)
    .execute(&pool)
    .await
    .expect("case fixture");

    let cases: Value = client
        .get(format!("{base_url}/admin/reconciliation-cases"))
        .bearer_auth(admin_token)
        .send()
        .await
        .expect("case list")
        .json()
        .await
        .expect("case list JSON");
    assert!(
        cases
            .as_array()
            .expect("case array")
            .iter()
            .any(|case| { case["job_id"].as_str() == Some(job_id.to_string().as_str()) })
    );
    let legacy_charge = json!({
        "resolution": "charge",
        "charge_microusd": 1,
        "note": "must not settle without evidence",
        "business_key": format!("contract-charge-{job_id}")
    });
    let response = client
        .post(format!(
            "{base_url}/admin/reconciliation-cases/{job_id}/refund"
        ))
        .bearer_auth(admin_token)
        .json(&legacy_charge)
        .send()
        .await
        .expect("legacy charge rejection");
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let refund = json!({
        "note": "contract refund",
        "business_key": format!("contract-refund-{job_id}")
    });
    for _ in 0..2 {
        let response = client
            .post(format!(
                "{base_url}/admin/reconciliation-cases/{job_id}/refund"
            ))
            .bearer_auth(admin_token)
            .json(&refund)
            .send()
            .await
            .expect("case resolution");
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }
    let row = sqlx::query(
        "SELECT j.state, h.status, h.amount_microusd, a.balance_microusd FROM generation.jobs j JOIN ledger.holds h ON h.job_id = j.id JOIN ledger.accounts a ON a.id = j.account_id WHERE j.id = $1",
    )
    .bind(job_id)
    .fetch_one(&pool)
    .await
    .expect("resolved state");
    assert_eq!(row.get::<String, _>("state"), "failed");
    assert_eq!(row.get::<String, _>("status"), "released");
    assert_eq!(
        row.get::<i64, _>("balance_microusd"),
        balance_after_hold + row.get::<i64, _>("amount_microusd")
    );
    let capture_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ledger.entries WHERE job_id = $1 AND kind = 'capture'",
    )
    .bind(job_id)
    .fetch_one(&pool)
    .await
    .expect("capture count");
    assert_eq!(capture_count, 0);
    let removed_charge_columns: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM information_schema.columns WHERE table_schema = 'operations' AND table_name = 'reconciliation_cases' AND column_name IN ('resolution', 'charge_microusd')",
    )
    .fetch_one(&pool)
    .await
    .expect("reconciliation schema");
    assert_eq!(removed_charge_columns, 0);
}

async fn verify_lease_recovery_contract(
    client: &Client,
    base_url: &str,
    api_key: &str,
    database_url: &str,
) {
    let leased_job: Value = client
        .post(format!("{base_url}/v1/image-generations"))
        .bearer_auth(api_key)
        .json(&generation_request(
            &format!("lease-before-submit-{}", Uuid::new_v4()),
            "lease recovery before submit",
        ))
        .send()
        .await
        .expect("leased recovery job")
        .json()
        .await
        .expect("leased recovery job JSON");
    let leased_job_id = Uuid::parse_str(leased_job["job_id"].as_str().expect("leased job id"))
        .expect("leased job UUID");
    let submitted_job: Value = client
        .post(format!("{base_url}/v1/image-generations"))
        .bearer_auth(api_key)
        .json(&generation_request(
            &format!("lease-after-submit-{}", Uuid::new_v4()),
            "lease recovery after submit",
        ))
        .send()
        .await
        .expect("submitted recovery job")
        .json()
        .await
        .expect("submitted recovery job JSON");
    let submitted_job_id =
        Uuid::parse_str(submitted_job["job_id"].as_str().expect("submitted job id"))
            .expect("submitted job UUID");
    let submitted_attempt_id = Uuid::new_v4();
    let pool = PgPool::connect(database_url)
        .await
        .expect("contract database");
    sqlx::query(
        "UPDATE generation.jobs SET state = 'leased', lease_owner = 'expired-worker', lease_expires_at = now() - interval '1 second' WHERE id = $1",
    )
    .bind(leased_job_id)
    .execute(&pool)
    .await
    .expect("expired leased fixture");
    sqlx::query(
        "UPDATE generation.jobs SET state = 'submitting', lease_owner = 'expired-worker', lease_expires_at = now() - interval '1 second' WHERE id = $1",
    )
    .bind(submitted_job_id)
    .execute(&pool)
    .await
    .expect("expired submission fixture");
    sqlx::query(
        "INSERT INTO generation.attempts (id, job_id, state, request_digest) VALUES ($1,$2,'submitting','lease-contract')",
    )
    .bind(submitted_attempt_id)
    .bind(submitted_job_id)
    .execute(&pool)
    .await
    .expect("submitted attempt fixture");

    let repository = PgHubRepository::connect(database_url, 2)
        .await
        .expect("recovery repository");
    let recovered = repository
        .recover_expired_leases()
        .await
        .expect("lease recovery");
    assert_eq!(recovered.returned_to_queue, 1);
    assert_eq!(recovered.sent_to_reconciliation, 1);
    let leased_state: String =
        sqlx::query_scalar("SELECT state FROM generation.jobs WHERE id = $1")
            .bind(leased_job_id)
            .fetch_one(&pool)
            .await
            .expect("leased recovery state");
    assert_eq!(leased_state, "accepted");
    let submitted_state: String =
        sqlx::query_scalar("SELECT state FROM generation.jobs WHERE id = $1")
            .bind(submitted_job_id)
            .fetch_one(&pool)
            .await
            .expect("submitted recovery state");
    assert_eq!(submitted_state, "reconciliation_required");
    let case_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM operations.reconciliation_cases WHERE job_id = $1",
    )
    .bind(submitted_job_id)
    .fetch_one(&pool)
    .await
    .expect("recovery case count");
    assert_eq!(case_count, 1);

    let repeated = repository
        .recover_expired_leases()
        .await
        .expect("repeated lease recovery");
    assert_eq!(repeated.returned_to_queue, 0);
    assert_eq!(repeated.sent_to_reconciliation, 0);
}
