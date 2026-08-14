//! Bounded website crawling behind Envoyage's own contract.
//!
//! Envoyage owns validation, limits, idempotency and the normalized result.
//! A configured engine does the fetch/render work. The first adapter targets an
//! unmodified Firecrawl v2 deployment; callers never see that provider's job
//! ids, pagination URLs or credentials.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read, Write},
    net::{IpAddr, SocketAddr, ToSocketAddrs},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use url::Url;

const MAX_ALLOWED_HOSTS: usize = 20;
const MAX_PATH_RULES: usize = 50;
const MAX_PATH_RULE_BYTES: usize = 256;
const MAX_PAGE_LIMIT: u32 = 2_000;
const MAX_ASSET_LIMIT: u32 = 20_000;
// A complete fashion Collection can contain more than a thousand original
// gallery images. Keep the ordinary 64 MiB default, but allow an explicitly
// authorized caller to request a still-bounded 1 GiB evidence snapshot.
const MAX_CONTENT_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_DURATION_SECS: u64 = 3_600;
const MAX_CONCURRENCY: u16 = 20;
const MAX_ASSET_BYTES: u64 = 25 * 1024 * 1024;
const MAX_ASSET_REDIRECTS: usize = 5;

fn generic_adapter_name() -> String {
    "generic".to_string()
}

fn crawl_adapter_version() -> String {
    "envoyage-crawl-v2".to_string()
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CrawlDiscovery {
    #[default]
    SitemapAndLinks,
    SitemapOnly,
    LinksOnly,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CrawlRenderPolicy {
    #[default]
    Auto,
    Static,
    Browser,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CrawlAdapter {
    #[default]
    Auto,
    Generic,
    ShopifyCollection,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CrawlLimits {
    #[serde(default = "default_max_pages")]
    pub max_pages: u32,
    #[serde(default = "default_max_depth")]
    pub max_depth: u16,
    #[serde(default = "default_max_assets")]
    pub max_assets: u32,
    #[serde(default = "default_max_content_bytes")]
    pub max_content_bytes: u64,
    #[serde(default = "default_max_duration_secs")]
    pub max_duration_secs: u64,
    #[serde(default = "default_max_concurrency")]
    pub max_concurrency: u16,
}

impl Default for CrawlLimits {
    fn default() -> Self {
        Self {
            max_pages: default_max_pages(),
            max_depth: default_max_depth(),
            max_assets: default_max_assets(),
            max_content_bytes: default_max_content_bytes(),
            max_duration_secs: default_max_duration_secs(),
            max_concurrency: default_max_concurrency(),
        }
    }
}

fn default_max_pages() -> u32 {
    500
}
fn default_max_depth() -> u16 {
    6
}
fn default_max_assets() -> u32 {
    5_000
}
fn default_max_content_bytes() -> u64 {
    64 * 1024 * 1024
}
fn default_max_duration_secs() -> u64 {
    900
}
fn default_max_concurrency() -> u16 {
    5
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CrawlCapture {
    #[serde(default = "yes")]
    pub sections: bool,
    #[serde(default = "yes")]
    pub links: bool,
    #[serde(default = "yes")]
    pub media: bool,
    #[serde(default)]
    pub markdown: bool,
    #[serde(default)]
    pub html: bool,
}

impl Default for CrawlCapture {
    fn default() -> Self {
        Self {
            sections: true,
            links: true,
            media: true,
            markdown: false,
            html: false,
        }
    }
}

fn yes() -> bool {
    true
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CrawlRequest {
    pub url: String,
    #[serde(default)]
    pub adapter: CrawlAdapter,
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
    #[serde(default)]
    pub include_paths: Vec<String>,
    #[serde(default)]
    pub exclude_paths: Vec<String>,
    #[serde(default)]
    pub discovery: CrawlDiscovery,
    #[serde(default)]
    pub render: CrawlRenderPolicy,
    #[serde(default)]
    pub capture: CrawlCapture,
    #[serde(default)]
    pub limits: CrawlLimits,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CrawlState {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CrawlProgress {
    pub completed_pages: u32,
    pub total_pages: u32,
    pub returned_pages: u32,
    pub returned_assets: u32,
    pub returned_content_bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CrawlSection {
    pub level: u8,
    pub heading: String,
    pub text: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CrawlMedia {
    pub id: String,
    pub position: u32,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CrawlPage {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub canonical_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub product_key: Option<String>,
    #[serde(default)]
    pub breadcrumbs: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_code: Option<u16>,
    #[serde(default)]
    pub sections: Vec<CrawlSection>,
    #[serde(default)]
    pub links: Vec<String>,
    #[serde(default)]
    pub media: Vec<CrawlMedia>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub markdown: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub html: Option<String>,
    pub content_sha256: String,
    pub truncated: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CrawlJob {
    pub id: String,
    pub adapter: String,
    pub adapter_version: String,
    pub state: CrawlState,
    pub request_fingerprint: String,
    pub created_at_ms: u64,
    pub progress: CrawlProgress,
    #[serde(default)]
    pub pages: Vec<CrawlPage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
pub struct CrawlAssetDownload {
    pub content_type: String,
    pub sha256: String,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Receipt {
    id: String,
    provider_id: String,
    #[serde(default = "generic_adapter_name")]
    adapter: String,
    #[serde(default = "crawl_adapter_version")]
    adapter_version: String,
    #[serde(default)]
    asset_hosts: Vec<String>,
    request_fingerprint: String,
    request: CrawlRequest,
    created_at_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct CachedAsset {
    content_type: String,
    sha256: String,
    byte_count: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CrawlAuditEvent<'a> {
    at_ms: u64,
    action: &'a str,
    returned_pages: u32,
    asset_id: Option<&'a str>,
}

#[derive(Clone, Debug)]
struct ProviderStart {
    id: String,
    adapter: String,
    adapter_version: String,
    asset_hosts: Vec<String>,
}

#[derive(Clone, Debug)]
struct ProviderStatus {
    state: CrawlState,
    completed: u32,
    total: u32,
    documents: Vec<Value>,
    next: Option<String>,
    warning: Option<String>,
    error: Option<String>,
}

trait CrawlProvider: Send + Sync {
    fn start(&self, request: &CrawlRequest, idempotency_key: &str)
    -> Result<ProviderStart, String>;
    fn read(&self, provider_id: &str, next: Option<&str>) -> Result<ProviderStatus, String>;
    fn cancel(&self, provider_id: &str) -> Result<(), String>;
    fn validate_cursor(&self, provider_id: &str, next: &str) -> bool;
}

struct FirecrawlProvider {
    base: Url,
    token: Option<String>,
    client: Client,
}

impl FirecrawlProvider {
    fn from_env() -> Result<Self, String> {
        let raw = std::env::var("ENVOYAGE_CRAWL_PROVIDER_URL").map_err(|_| {
            "crawling is not configured: set ENVOYAGE_CRAWL_PROVIDER_URL".to_string()
        })?;
        let mut base =
            Url::parse(&raw).map_err(|e| format!("invalid ENVOYAGE_CRAWL_PROVIDER_URL: {e}"))?;
        if !matches!(base.scheme(), "http" | "https") {
            return Err("ENVOYAGE_CRAWL_PROVIDER_URL must use http or https".to_string());
        }
        let base_path = base.path().trim_end_matches('/').to_string();
        base.set_path(&base_path);
        let token = std::env::var("ENVOYAGE_CRAWL_PROVIDER_TOKEN")
            .ok()
            .filter(|v| !v.is_empty());
        let client = Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|e| format!("build crawl provider client: {e}"))?;
        Ok(Self {
            base,
            token,
            client,
        })
    }

    fn request(&self, method: reqwest::Method, url: Url) -> reqwest::blocking::RequestBuilder {
        let builder = self.client.request(method, url);
        match &self.token {
            Some(token) => builder.bearer_auth(token),
            None => builder,
        }
    }

    fn endpoint(&self, path: &str) -> Result<Url, String> {
        let mut url = self.base.clone();
        let base_path = self.base.path().trim_end_matches('/');
        let path = path.trim_start_matches('/');
        url.set_path(&format!("{base_path}/{path}"));
        url.set_query(None);
        url.set_fragment(None);
        Ok(url)
    }
}

impl CrawlProvider for FirecrawlProvider {
    fn start(
        &self,
        request: &CrawlRequest,
        idempotency_key: &str,
    ) -> Result<ProviderStart, String> {
        if request.render == CrawlRenderPolicy::Static {
            return Err(
                "configured crawl provider cannot guarantee render=static; use auto or browser"
                    .to_string(),
            );
        }
        let sitemap = match request.discovery {
            CrawlDiscovery::SitemapAndLinks => "include",
            CrawlDiscovery::SitemapOnly => "only",
            CrawlDiscovery::LinksOnly => "skip",
        };
        let mut formats = Vec::new();
        if request.capture.markdown || request.capture.sections {
            formats.push("markdown");
        }
        if request.capture.html {
            formats.push("rawHtml");
        }
        if request.capture.links {
            formats.push("links");
        }
        if request.capture.media {
            formats.push("images");
        }
        let mut scrape_options = json!({
            "formats": formats,
            "timeout": request.limits.max_duration_secs.saturating_mul(1000).min(120_000),
        });
        if request.render == CrawlRenderPolicy::Browser {
            scrape_options["waitFor"] = json!(1_000);
        }
        let body = json!({
            "url": request.url,
            "includePaths": request.include_paths,
            "excludePaths": request.exclude_paths,
            "maxDiscoveryDepth": request.limits.max_depth,
            "limit": request.limits.max_pages,
            "allowExternalLinks": false,
            "allowSubdomains": request.allowed_hosts.iter().any(|h| {
                Url::parse(&request.url).ok().and_then(|u| u.host_str().map(|seed| h != seed)).unwrap_or(false)
            }),
            "sitemap": sitemap,
            "maxConcurrency": request.limits.max_concurrency,
            "scrapeOptions": scrape_options,
            "zeroDataRetention": true,
            "integration": "envoyage",
        });
        let response = self
            .request(reqwest::Method::POST, self.endpoint("v2/crawl")?)
            .header("x-idempotency-key", idempotency_key)
            .json(&body)
            .send()
            .map_err(|e| format!("start crawl provider job: {e}"))?;
        let status = response.status();
        let value: Value = response
            .json()
            .map_err(|e| format!("read crawl provider response: {e}"))?;
        if !status.is_success() || value.get("success").and_then(Value::as_bool) != Some(true) {
            return Err(provider_error(status.as_u16(), &value));
        }
        let id = value
            .get("id")
            .and_then(Value::as_str)
            .ok_or("crawl provider returned no job id")?;
        validate_job_id(id)?;
        Ok(ProviderStart {
            id: id.to_string(),
            adapter: "generic".to_string(),
            adapter_version: crawl_adapter_version(),
            asset_hosts: request.allowed_hosts.clone(),
        })
    }

    fn read(&self, provider_id: &str, next: Option<&str>) -> Result<ProviderStatus, String> {
        validate_job_id(provider_id)?;
        let url = match next {
            Some(next) if self.validate_cursor(provider_id, next) => {
                Url::parse(next).map_err(|e| e.to_string())?
            }
            Some(_) => return Err("invalid crawl cursor".to_string()),
            None => self.endpoint(&format!("v2/crawl/{provider_id}"))?,
        };
        let response = self
            .request(reqwest::Method::GET, url)
            .send()
            .map_err(|e| format!("read crawl provider job: {e}"))?;
        let status = response.status();
        let value: Value = response
            .json()
            .map_err(|e| format!("read crawl provider response: {e}"))?;
        if !status.is_success() || value.get("success").and_then(Value::as_bool) == Some(false) {
            return Err(provider_error(status.as_u16(), &value));
        }
        let state = match value
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("scraping")
        {
            "completed" => CrawlState::Completed,
            "failed" => CrawlState::Failed,
            "cancelled" => CrawlState::Cancelled,
            "queued" => CrawlState::Queued,
            _ => CrawlState::Running,
        };
        Ok(ProviderStatus {
            state,
            completed: u32_value(value.get("completed")),
            total: u32_value(value.get("total")),
            documents: value
                .get("data")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
            next: value
                .get("next")
                .and_then(Value::as_str)
                .map(str::to_string),
            warning: value
                .get("warning")
                .and_then(Value::as_str)
                .map(str::to_string),
            error: value
                .get("error")
                .and_then(Value::as_str)
                .map(str::to_string),
        })
    }

    fn cancel(&self, provider_id: &str) -> Result<(), String> {
        validate_job_id(provider_id)?;
        let response = self
            .request(
                reqwest::Method::DELETE,
                self.endpoint(&format!("v2/crawl/{provider_id}"))?,
            )
            .send()
            .map_err(|e| format!("cancel crawl provider job: {e}"))?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(format!(
                "crawl provider refused cancellation: HTTP {}",
                response.status().as_u16()
            ))
        }
    }

    fn validate_cursor(&self, provider_id: &str, next: &str) -> bool {
        let Ok(url) = Url::parse(next) else {
            return false;
        };
        url.scheme() == self.base.scheme()
            && url.host_str() == self.base.host_str()
            && url.port_or_known_default() == self.base.port_or_known_default()
            && url.path().contains(&format!("/crawl/{provider_id}"))
    }
}

#[derive(Debug, Deserialize)]
struct ShopifyFeed {
    #[serde(default)]
    products: Vec<ShopifyProduct>,
}

#[derive(Debug, Deserialize, Serialize)]
struct ShopifyProduct {
    id: u64,
    title: String,
    handle: String,
    #[serde(default)]
    body_html: String,
    #[serde(default)]
    images: Vec<ShopifyImage>,
}

#[derive(Debug, Deserialize, Serialize)]
struct ShopifyImage {
    #[serde(default)]
    position: u32,
    src: String,
    #[serde(default)]
    width: u32,
    #[serde(default)]
    height: u32,
    alt: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct ShopifySnapshot {
    documents: Vec<Value>,
    asset_hosts: Vec<String>,
    warning: Option<String>,
}

struct ShopifyCollectionProvider {
    state_dir: PathBuf,
    client: Client,
    resolve_dns: bool,
}

impl ShopifyCollectionProvider {
    fn new(state_dir: PathBuf, resolve_dns: bool) -> Result<Self, String> {
        let client = Client::builder()
            .timeout(Duration::from_secs(60))
            .user_agent("Envoyage/0.2 (+https://github.com/Envoyage-Browsing/envoyage)")
            .build()
            .map_err(|error| format!("build Shopify collection client: {error}"))?;
        Ok(Self {
            state_dir,
            client,
            resolve_dns,
        })
    }

    fn feed_url(request: &CrawlRequest, page: u32) -> Result<Url, String> {
        let mut url = Url::parse(&request.url).map_err(|error| error.to_string())?;
        let segments = url
            .path_segments()
            .ok_or("Shopify collection URL has no path")?
            .filter(|segment| !segment.is_empty())
            .collect::<Vec<_>>();
        let Some(collection_index) = segments
            .iter()
            .position(|segment| *segment == "collections")
        else {
            return Err("Shopify adapter needs a /collections/{handle} URL".to_string());
        };
        if segments.get(collection_index + 1).is_none() {
            return Err("Shopify adapter needs a collection handle".to_string());
        }
        let prefix = segments[..=collection_index + 1].join("/");
        url.set_path(&format!("/{prefix}/products.json"));
        url.set_query(Some(&format!("limit=250&page={page}")));
        url.set_fragment(None);
        Ok(url)
    }

    fn snapshot_path(&self, id: &str) -> PathBuf {
        self.state_dir.join(format!("shopify-{id}.json"))
    }

    fn supports(request: &CrawlRequest) -> bool {
        Self::feed_url(request, 1).is_ok()
    }
}

impl CrawlProvider for ShopifyCollectionProvider {
    fn start(
        &self,
        request: &CrawlRequest,
        idempotency_key: &str,
    ) -> Result<ProviderStart, String> {
        let provider_id = format!(
            "shopify_{}",
            &sha256_text(&format!("{}\0{idempotency_key}", request.url))[..32]
        );
        let path = self.snapshot_path(&provider_id);
        if path.exists() {
            let snapshot: ShopifySnapshot = read_json(&path, "Shopify collection snapshot")?;
            return Ok(ProviderStart {
                id: provider_id,
                adapter: "shopify_collection".to_string(),
                adapter_version: crawl_adapter_version(),
                asset_hosts: snapshot.asset_hosts,
            });
        }

        let seed = Url::parse(&request.url).map_err(|error| error.to_string())?;
        let seed_host = seed
            .host_str()
            .ok_or("Shopify collection URL has no host")?;
        let mut products = Vec::new();
        let mut page = 1_u32;
        let mut exceeded_page_limit = false;
        loop {
            let feed_url = Self::feed_url(request, page)?;
            if self.resolve_dns {
                ensure_public_dns(seed_host)?;
            }
            let response = self
                .client
                .get(feed_url)
                .send()
                .map_err(|error| format!("read Shopify collection: {error}"))?;
            if !response.status().is_success() {
                return Err(format!(
                    "Shopify collection feed returned HTTP {}",
                    response.status().as_u16()
                ));
            }
            let feed: ShopifyFeed = response
                .json()
                .map_err(|error| format!("decode Shopify collection feed: {error}"))?;
            if feed.products.is_empty() {
                break;
            }
            for product in feed.products {
                if products.len() >= request.limits.max_pages as usize {
                    exceeded_page_limit = true;
                    break;
                }
                products.push(product);
            }
            if exceeded_page_limit || products.len() % 250 != 0 {
                break;
            }
            page = page.saturating_add(1);
        }
        if products.is_empty() {
            return Err("Shopify collection contained no Products".to_string());
        }

        let image_count = products
            .iter()
            .map(|product| product.images.len())
            .sum::<usize>();
        if image_count > request.limits.max_assets as usize {
            return Err(format!(
                "Shopify collection has {image_count} images, above maxAssets {}",
                request.limits.max_assets
            ));
        }

        let mut asset_hosts = BTreeSet::new();
        let mut documents = Vec::with_capacity(products.len());
        for product in products {
            let product_url = seed
                .join(&format!("/products/{}", product.handle))
                .map_err(|error| format!("build Shopify Product URL: {error}"))?;
            let media = product
                .images
                .iter()
                .map(|image| {
                    let image_url = Url::parse(&image.src)
                        .map_err(|error| format!("invalid Shopify image URL: {error}"))?;
                    if !matches!(image_url.scheme(), "http" | "https") {
                        return Err("Shopify image URL must use http or https".to_string());
                    }
                    let host = image_url
                        .host_str()
                        .ok_or("Shopify image URL has no host")?
                        .to_ascii_lowercase();
                    if self.resolve_dns {
                        ensure_public_dns(&host)?;
                    }
                    asset_hosts.insert(host);
                    Ok(json!({
                        "url": image_url.to_string(),
                        "position": image.position.saturating_sub(1),
                        "alt": image.alt,
                        "width": image.width,
                        "height": image.height,
                    }))
                })
                .collect::<Result<Vec<_>, String>>()?;
            let canonical = product_url.to_string();
            let product_key = if product.handle.is_empty() {
                product.id.to_string()
            } else {
                product.handle.clone()
            };
            let content_sha256 = sha256_json(&product)?;
            documents.push(json!({
                "markdown": product.body_html,
                "breadcrumbs": ["Collection", product.title],
                "envoyageMedia": media,
                "metadata": {
                    "sourceURL": canonical,
                    "canonicalUrl": canonical,
                    "title": product.title,
                    "statusCode": 200,
                    "pageType": "product",
                    "productKey": product_key,
                    "contentSha256": content_sha256,
                }
            }));
        }
        let snapshot = ShopifySnapshot {
            documents,
            asset_hosts: asset_hosts.into_iter().collect(),
            warning: exceeded_page_limit.then(|| {
                format!(
                    "Shopify collection exceeded maxPages {}; remaining Products were not imported",
                    request.limits.max_pages
                )
            }),
        };
        write_json(&path, &snapshot, "Shopify collection snapshot")?;
        Ok(ProviderStart {
            id: provider_id,
            adapter: "shopify_collection".to_string(),
            adapter_version: crawl_adapter_version(),
            asset_hosts: snapshot.asset_hosts,
        })
    }

    fn read(&self, provider_id: &str, next: Option<&str>) -> Result<ProviderStatus, String> {
        let snapshot: ShopifySnapshot = read_json(
            &self.snapshot_path(provider_id),
            "Shopify collection snapshot",
        )?;
        let offset = match next {
            None => 0,
            Some(next) => next
                .strip_prefix(&format!("shopify:{provider_id}:"))
                .and_then(|value| value.parse::<usize>().ok())
                .ok_or("invalid Shopify cursor")?,
        };
        let end = offset.saturating_add(50).min(snapshot.documents.len());
        let documents = snapshot.documents[offset..end].to_vec();
        Ok(ProviderStatus {
            state: CrawlState::Completed,
            completed: snapshot.documents.len() as u32,
            total: snapshot.documents.len() as u32,
            documents,
            next: (end < snapshot.documents.len()).then(|| format!("shopify:{provider_id}:{end}")),
            warning: snapshot.warning,
            error: None,
        })
    }

    fn cancel(&self, _provider_id: &str) -> Result<(), String> {
        Ok(())
    }

    fn validate_cursor(&self, provider_id: &str, next: &str) -> bool {
        next.strip_prefix(&format!("shopify:{provider_id}:"))
            .and_then(|value| value.parse::<usize>().ok())
            .is_some()
    }
}

struct CrawlProviderRouter {
    generic: Option<FirecrawlProvider>,
    shopify: ShopifyCollectionProvider,
}

impl CrawlProviderRouter {
    fn from_env(state_dir: PathBuf, resolve_dns: bool) -> Result<Self, String> {
        Ok(Self {
            generic: FirecrawlProvider::from_env().ok(),
            shopify: ShopifyCollectionProvider::new(state_dir, resolve_dns)?,
        })
    }
}

impl CrawlProvider for CrawlProviderRouter {
    fn start(
        &self,
        request: &CrawlRequest,
        idempotency_key: &str,
    ) -> Result<ProviderStart, String> {
        let try_shopify = matches!(
            request.adapter,
            CrawlAdapter::Auto | CrawlAdapter::ShopifyCollection
        ) && ShopifyCollectionProvider::supports(request);
        if try_shopify {
            match self.shopify.start(request, idempotency_key) {
                Ok(started) => return Ok(started),
                Err(error) if request.adapter == CrawlAdapter::ShopifyCollection => {
                    return Err(error);
                }
                Err(_) => {}
            }
        }
        let generic = self.generic.as_ref().ok_or(
            "crawling is not configured: this URL has no verified site adapter and ENVOYAGE_CRAWL_PROVIDER_URL is not set",
        )?;
        generic.start(request, idempotency_key)
    }

    fn read(&self, provider_id: &str, next: Option<&str>) -> Result<ProviderStatus, String> {
        if provider_id.starts_with("shopify_") {
            self.shopify.read(provider_id, next)
        } else {
            self.generic
                .as_ref()
                .ok_or("generic crawl provider is not configured")?
                .read(provider_id, next)
        }
    }

    fn cancel(&self, provider_id: &str) -> Result<(), String> {
        if provider_id.starts_with("shopify_") {
            self.shopify.cancel(provider_id)
        } else {
            self.generic
                .as_ref()
                .ok_or("generic crawl provider is not configured")?
                .cancel(provider_id)
        }
    }

    fn validate_cursor(&self, provider_id: &str, next: &str) -> bool {
        if provider_id.starts_with("shopify_") {
            self.shopify.validate_cursor(provider_id, next)
        } else {
            self.generic
                .as_ref()
                .is_some_and(|provider| provider.validate_cursor(provider_id, next))
        }
    }
}

pub struct CrawlService {
    provider: Arc<dyn CrawlProvider>,
    state_dir: PathBuf,
    lock: Mutex<()>,
    resolve_dns: bool,
}

impl CrawlService {
    fn from_env() -> Result<Self, String> {
        let state_dir = std::env::var_os("ENVOYAGE_CRAWL_STATE_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| envoyage_home().join("crawls"));
        let provider = Arc::new(CrawlProviderRouter::from_env(state_dir.clone(), true)?);
        Self::new(provider, state_dir, true)
    }

    fn new(
        provider: Arc<dyn CrawlProvider>,
        state_dir: PathBuf,
        resolve_dns: bool,
    ) -> Result<Self, String> {
        fs::create_dir_all(&state_dir).map_err(|e| format!("create crawl state directory: {e}"))?;
        Ok(Self {
            provider,
            state_dir,
            lock: Mutex::new(()),
            resolve_dns,
        })
    }

    pub fn start(&self, request: CrawlRequest, idempotency_key: &str) -> Result<CrawlJob, String> {
        let request = validate_request(request, self.resolve_dns)?;
        validate_idempotency_key(idempotency_key)?;
        let request_fingerprint = sha256_json(&request)?;
        let _guard = self
            .lock
            .lock()
            .map_err(|_| "crawl state lock poisoned".to_string())?;
        let idem_path = self.idempotency_path(idempotency_key);
        if idem_path.exists() {
            let receipt = read_receipt(&idem_path)?;
            if receipt.request_fingerprint != request_fingerprint {
                return Err(
                    "idempotency key was already used for a different crawl request".to_string(),
                );
            }
            return Ok(empty_job(&receipt, CrawlState::Queued));
        }
        let provider = self.provider.start(&request, idempotency_key)?;
        let created_at_ms = now_ms();
        let id = envoyage_job_id(&provider.id, &request_fingerprint, created_at_ms);
        let receipt = Receipt {
            id,
            provider_id: provider.id,
            adapter: provider.adapter,
            adapter_version: provider.adapter_version,
            asset_hosts: provider.asset_hosts,
            request_fingerprint,
            request,
            created_at_ms,
        };
        write_receipt(&self.job_path(&receipt.id), &receipt)?;
        write_receipt(&idem_path, &receipt)?;
        append_json_line(
            &self.audit_path(&receipt.id),
            &CrawlAuditEvent {
                at_ms: now_ms(),
                action: "start",
                returned_pages: 0,
                asset_id: None,
            },
            "crawl audit",
        )?;
        Ok(empty_job(&receipt, CrawlState::Queued))
    }

    pub fn read(&self, id: &str, cursor: Option<&str>) -> Result<CrawlJob, String> {
        validate_job_id(id)?;
        let receipt = read_receipt(&self.job_path(id))?;
        let next = match cursor {
            Some(cursor) => Some(decode_cursor(cursor)?),
            None => None,
        };
        if let Some(next) = next.as_deref()
            && !self.provider.validate_cursor(&receipt.provider_id, next)
        {
            return Err("invalid crawl cursor".to_string());
        }
        let elapsed = now_ms().saturating_sub(receipt.created_at_ms) / 1000;
        let mut status = self.provider.read(&receipt.provider_id, next.as_deref())?;
        if matches!(status.state, CrawlState::Queued | CrawlState::Running)
            && elapsed > receipt.request.limits.max_duration_secs
        {
            self.provider.cancel(&receipt.provider_id)?;
            status.state = CrawlState::Cancelled;
            status.error = Some("crawl exceeded maxDurationSecs and was cancelled".to_string());
        }
        let job = normalize_job(&receipt, status)?;
        self.record_asset_manifest(&receipt, &job.pages)?;
        self.append_audit(
            &receipt.id,
            CrawlAuditEvent {
                at_ms: now_ms(),
                action: "read",
                returned_pages: job.progress.returned_pages,
                asset_id: None,
            },
        )?;
        Ok(job)
    }

    pub fn cancel(&self, id: &str) -> Result<CrawlJob, String> {
        validate_job_id(id)?;
        let receipt = read_receipt(&self.job_path(id))?;
        self.provider.cancel(&receipt.provider_id)?;
        self.append_audit(
            id,
            CrawlAuditEvent {
                at_ms: now_ms(),
                action: "cancel",
                returned_pages: 0,
                asset_id: None,
            },
        )?;
        Ok(empty_job(&receipt, CrawlState::Cancelled))
    }

    pub fn download_asset(&self, id: &str, asset_id: &str) -> Result<CrawlAssetDownload, String> {
        validate_job_id(id)?;
        validate_asset_id(asset_id)?;
        let receipt = read_receipt(&self.job_path(id))?;
        if let Some(cached) = self.read_cached_asset(id, asset_id)? {
            return Ok(cached);
        }
        let manifest = self.read_asset_manifest(id)?;
        let raw_url = manifest
            .get(asset_id)
            .ok_or("crawl asset not found; read the result page containing it first")?;
        let allowed = receipt.asset_hosts.iter().cloned().collect::<BTreeSet<_>>();
        let max_bytes = receipt
            .request
            .limits
            .max_content_bytes
            .min(MAX_ASSET_BYTES);
        let download = download_public_image(raw_url, &allowed, max_bytes)?;
        self.cache_asset(&receipt, asset_id, &download)?;
        self.append_audit(
            id,
            CrawlAuditEvent {
                at_ms: now_ms(),
                action: "download_asset",
                returned_pages: 0,
                asset_id: Some(asset_id),
            },
        )?;
        Ok(download)
    }

    fn job_path(&self, id: &str) -> PathBuf {
        self.state_dir.join(format!("job-{id}.json"))
    }

    fn idempotency_path(&self, key: &str) -> PathBuf {
        self.state_dir
            .join(format!("idempotency-{}.json", sha256_text(key)))
    }

    fn asset_manifest_path(&self, id: &str) -> PathBuf {
        self.state_dir.join(format!("assets-{id}.json"))
    }

    fn audit_path(&self, id: &str) -> PathBuf {
        self.state_dir.join(format!("audit-{id}.jsonl"))
    }

    fn append_audit(&self, id: &str, event: CrawlAuditEvent<'_>) -> Result<(), String> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| "crawl state lock poisoned".to_string())?;
        append_json_line(&self.audit_path(id), &event, "crawl audit")
    }

    fn cached_asset_path(&self, id: &str, asset_id: &str) -> PathBuf {
        self.state_dir.join(format!("asset-{id}-{asset_id}.bin"))
    }

    fn cached_asset_meta_path(&self, id: &str, asset_id: &str) -> PathBuf {
        self.state_dir.join(format!("asset-{id}-{asset_id}.json"))
    }

    fn record_asset_manifest(&self, receipt: &Receipt, pages: &[CrawlPage]) -> Result<(), String> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| "crawl state lock poisoned".to_string())?;
        let path = self.asset_manifest_path(&receipt.id);
        let mut manifest: BTreeMap<String, String> = if path.exists() {
            read_json(&path, "crawl asset manifest")?
        } else {
            BTreeMap::new()
        };
        for media in pages.iter().flat_map(|page| page.media.iter()) {
            if manifest.len() >= receipt.request.limits.max_assets as usize
                && !manifest.contains_key(&media.id)
            {
                return Err("crawl asset manifest exceeded maxAssets".to_string());
            }
            match manifest.get(&media.id) {
                Some(existing) if existing != &media.url => {
                    return Err("crawl provider changed an existing asset identity".to_string());
                }
                _ => {
                    manifest.insert(media.id.clone(), media.url.clone());
                }
            }
        }
        write_json(&path, &manifest, "crawl asset manifest")
    }

    fn read_asset_manifest(&self, id: &str) -> Result<BTreeMap<String, String>, String> {
        read_json(&self.asset_manifest_path(id), "crawl asset manifest")
    }

    fn read_cached_asset(
        &self,
        id: &str,
        asset_id: &str,
    ) -> Result<Option<CrawlAssetDownload>, String> {
        let meta_path = self.cached_asset_meta_path(id, asset_id);
        let bytes_path = self.cached_asset_path(id, asset_id);
        if !meta_path.exists() && !bytes_path.exists() {
            return Ok(None);
        }
        if !meta_path.exists() || !bytes_path.exists() {
            if meta_path.exists() {
                fs::remove_file(&meta_path)
                    .map_err(|e| format!("repair cached crawl asset metadata: {e}"))?;
            }
            if bytes_path.exists() {
                fs::remove_file(&bytes_path)
                    .map_err(|e| format!("repair cached crawl asset bytes: {e}"))?;
            }
            return Ok(None);
        }
        let meta: CachedAsset = read_json(&meta_path, "cached crawl asset metadata")?;
        let bytes = fs::read(&bytes_path).map_err(|e| format!("read cached crawl asset: {e}"))?;
        if bytes.len() as u64 != meta.byte_count || sha256_bytes(&bytes) != meta.sha256 {
            return Err("cached crawl asset failed its integrity check".to_string());
        }
        Ok(Some(CrawlAssetDownload {
            content_type: meta.content_type,
            sha256: meta.sha256,
            bytes,
        }))
    }

    fn cache_asset(
        &self,
        receipt: &Receipt,
        asset_id: &str,
        download: &CrawlAssetDownload,
    ) -> Result<(), String> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| "crawl state lock poisoned".to_string())?;
        if self.read_cached_asset(&receipt.id, asset_id)?.is_some() {
            return Ok(());
        }
        let mut used = 0_u64;
        for entry in
            fs::read_dir(&self.state_dir).map_err(|e| format!("read crawl state directory: {e}"))?
        {
            let entry = entry.map_err(|e| format!("read crawl state entry: {e}"))?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with(&format!("asset-{}-", receipt.id)) && name.ends_with(".json") {
                let meta: CachedAsset = read_json(&entry.path(), "cached crawl asset metadata")?;
                used = used.saturating_add(meta.byte_count);
            }
        }
        if used.saturating_add(download.bytes.len() as u64)
            > receipt.request.limits.max_content_bytes
        {
            return Err("crawl asset downloads exceeded maxContentBytes".to_string());
        }
        write_bytes_atomic(
            &self.cached_asset_path(&receipt.id, asset_id),
            &download.bytes,
            "cached crawl asset",
        )?;
        let meta = CachedAsset {
            content_type: download.content_type.clone(),
            sha256: download.sha256.clone(),
            byte_count: download.bytes.len() as u64,
        };
        write_json(
            &self.cached_asset_meta_path(&receipt.id, asset_id),
            &meta,
            "cached crawl asset metadata",
        )
    }
}

static SERVICE: OnceLock<Result<CrawlService, String>> = OnceLock::new();

pub fn service() -> Result<&'static CrawlService, String> {
    SERVICE
        .get_or_init(CrawlService::from_env)
        .as_ref()
        .map_err(Clone::clone)
}

fn validate_request(mut request: CrawlRequest, resolve_dns: bool) -> Result<CrawlRequest, String> {
    let seed = validate_public_url(&request.url, resolve_dns)?;
    let seed_host = seed
        .host_str()
        .ok_or("crawl URL must have a host")?
        .to_ascii_lowercase();
    request.url = seed.to_string();
    if request.allowed_hosts.is_empty() {
        request.allowed_hosts.push(seed_host.clone());
    }
    if request.allowed_hosts.len() > MAX_ALLOWED_HOSTS {
        return Err(format!(
            "allowedHosts may contain at most {MAX_ALLOWED_HOSTS} hosts"
        ));
    }
    let mut hosts = BTreeSet::new();
    for host in &request.allowed_hosts {
        let host = normalize_host(host)?;
        if resolve_dns {
            ensure_public_dns(&host)?;
        }
        hosts.insert(host);
    }
    if !hosts.contains(&seed_host) {
        return Err("allowedHosts must include the crawl URL host".to_string());
    }
    request.allowed_hosts = hosts.into_iter().collect();
    validate_path_rules("includePaths", &request.include_paths)?;
    validate_path_rules("excludePaths", &request.exclude_paths)?;
    let limits = &request.limits;
    if !(1..=MAX_PAGE_LIMIT).contains(&limits.max_pages) {
        return Err(format!("maxPages must be between 1 and {MAX_PAGE_LIMIT}"));
    }
    if limits.max_depth > 20 {
        return Err("maxDepth must be at most 20".to_string());
    }
    if !(1..=MAX_ASSET_LIMIT).contains(&limits.max_assets) {
        return Err(format!("maxAssets must be between 1 and {MAX_ASSET_LIMIT}"));
    }
    if !(1..=MAX_CONTENT_BYTES).contains(&limits.max_content_bytes) {
        return Err(format!(
            "maxContentBytes must be between 1 and {MAX_CONTENT_BYTES}"
        ));
    }
    if !(1..=MAX_DURATION_SECS).contains(&limits.max_duration_secs) {
        return Err(format!(
            "maxDurationSecs must be between 1 and {MAX_DURATION_SECS}"
        ));
    }
    if !(1..=MAX_CONCURRENCY).contains(&limits.max_concurrency) {
        return Err(format!(
            "maxConcurrency must be between 1 and {MAX_CONCURRENCY}"
        ));
    }
    Ok(request)
}

fn validate_public_url(raw: &str, resolve_dns: bool) -> Result<Url, String> {
    let url = Url::parse(raw).map_err(|e| format!("invalid crawl URL: {e}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("crawl URL must use http or https".to_string());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("crawl URL must not contain credentials".to_string());
    }
    if url.fragment().is_some() {
        return Err("crawl URL must not contain a fragment".to_string());
    }
    if let Some(port) = url.port()
        && port != 80
        && port != 443
    {
        return Err("crawl URL may use only port 80 or 443".to_string());
    }
    let host = url.host_str().ok_or("crawl URL must have a host")?;
    normalize_host(host)?;
    if resolve_dns {
        ensure_public_dns(host)?;
    }
    Ok(url)
}

fn normalize_host(raw: &str) -> Result<String, String> {
    if raw.is_empty()
        || raw.len() > 253
        || raw.contains('/')
        || raw.contains('@')
        || raw.contains(':')
    {
        return Err(format!("invalid allowed host {raw:?}"));
    }
    let host = raw.trim_end_matches('.').to_ascii_lowercase();
    if host == "localhost" || host.ends_with(".localhost") || host.ends_with(".local") {
        return Err("local hosts are not allowed".to_string());
    }
    if let Ok(ip) = host.parse::<IpAddr>()
        && !is_public_ip(ip)
    {
        return Err("private or reserved IP addresses are not allowed".to_string());
    }
    Ok(host)
}

fn ensure_public_dns(host: &str) -> Result<(), String> {
    let addrs = (host, 443)
        .to_socket_addrs()
        .map_err(|e| format!("resolve crawl host {host}: {e}"))?
        .map(|a| a.ip())
        .collect::<Vec<_>>();
    if addrs.is_empty() {
        return Err(format!("crawl host {host} did not resolve"));
    }
    if addrs.iter().any(|ip| !is_public_ip(*ip)) {
        return Err(format!(
            "crawl host {host} resolves to a private or reserved address"
        ));
    }
    Ok(())
}

fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let octets = ip.octets();
            !ip.is_private()
                && !ip.is_loopback()
                && !ip.is_link_local()
                && !ip.is_multicast()
                && !ip.is_unspecified()
                && octets[0] != 0
                && octets[0] < 224
                && !(octets[0] == 100 && (64..=127).contains(&octets[1]))
                && !(octets[0] == 192 && octets[1] == 0)
                && !(octets[0] == 198 && (octets[1] == 18 || octets[1] == 19))
        }
        IpAddr::V6(ip) => {
            !ip.is_loopback()
                && !ip.is_multicast()
                && !ip.is_unspecified()
                && (ip.segments()[0] & 0xfe00 != 0xfc00)
                && (ip.segments()[0] & 0xffc0 != 0xfe80)
        }
    }
}

fn validate_path_rules(name: &str, rules: &[String]) -> Result<(), String> {
    if rules.len() > MAX_PATH_RULES {
        return Err(format!(
            "{name} may contain at most {MAX_PATH_RULES} entries"
        ));
    }
    for rule in rules {
        if rule.is_empty()
            || rule.len() > MAX_PATH_RULE_BYTES
            || rule.contains('\0')
            || rule.contains('\n')
        {
            return Err(format!("{name} contains an invalid rule"));
        }
    }
    Ok(())
}

fn normalize_job(receipt: &Receipt, status: ProviderStatus) -> Result<CrawlJob, String> {
    let allowed = receipt
        .request
        .allowed_hosts
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let allowed_assets = receipt.asset_hosts.iter().cloned().collect::<BTreeSet<_>>();
    let mut pages = Vec::new();
    let mut warnings = status.warning.into_iter().collect::<Vec<_>>();
    let mut returned_bytes = 0_u64;
    let mut returned_assets = 0_u32;
    for doc in status
        .documents
        .into_iter()
        .take(receipt.request.limits.max_pages as usize)
    {
        let Some(page) = normalize_document(
            doc,
            &receipt.request,
            &allowed,
            &allowed_assets,
            &mut returned_bytes,
            &mut returned_assets,
            &mut warnings,
        )?
        else {
            continue;
        };
        pages.push(page);
    }
    let next_cursor = status.next.map(|next| encode_cursor(&next));
    Ok(CrawlJob {
        id: receipt.id.clone(),
        adapter: receipt.adapter.clone(),
        adapter_version: receipt.adapter_version.clone(),
        state: status.state,
        request_fingerprint: receipt.request_fingerprint.clone(),
        created_at_ms: receipt.created_at_ms,
        progress: CrawlProgress {
            completed_pages: status.completed,
            total_pages: status.total,
            returned_pages: pages.len() as u32,
            returned_assets,
            returned_content_bytes: returned_bytes,
        },
        pages,
        next_cursor,
        warnings,
        error: status.error,
    })
}

fn normalize_document(
    doc: Value,
    request: &CrawlRequest,
    allowed: &BTreeSet<String>,
    allowed_assets: &BTreeSet<String>,
    returned_bytes: &mut u64,
    returned_assets: &mut u32,
    warnings: &mut Vec<String>,
) -> Result<Option<CrawlPage>, String> {
    let meta = doc.get("metadata").and_then(Value::as_object);
    let raw_url = meta
        .and_then(|m| m.get("sourceURL").or_else(|| m.get("url")))
        .and_then(Value::as_str)
        .or_else(|| doc.get("url").and_then(Value::as_str));
    let Some(raw_url) = raw_url else {
        warnings.push("provider returned a page without a URL; it was omitted".to_string());
        return Ok(None);
    };
    let Ok(url) = Url::parse(raw_url) else {
        warnings.push("provider returned an invalid page URL; it was omitted".to_string());
        return Ok(None);
    };
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    if !matches!(url.scheme(), "http" | "https") || !allowed.contains(&host) {
        warnings.push(format!("page outside allowedHosts was omitted: {raw_url}"));
        return Ok(None);
    }
    let markdown_raw = doc.get("markdown").and_then(Value::as_str).unwrap_or("");
    let html_raw = doc
        .get("rawHtml")
        .or_else(|| doc.get("html"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let digest_source = if !html_raw.is_empty() {
        html_raw
    } else {
        markdown_raw
    };
    let content_sha256 = metadata_string(meta, "contentSha256")
        .filter(|value| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .unwrap_or_else(|| sha256_text(digest_source));
    let mut truncated = false;
    let markdown = if request.capture.markdown {
        take_content(
            markdown_raw,
            returned_bytes,
            request.limits.max_content_bytes,
            &mut truncated,
        )
    } else {
        None
    };
    let html = if request.capture.html {
        take_content(
            html_raw,
            returned_bytes,
            request.limits.max_content_bytes,
            &mut truncated,
        )
    } else {
        None
    };
    let sections = if request.capture.sections {
        markdown_sections(
            markdown_raw,
            returned_bytes,
            request.limits.max_content_bytes,
            &mut truncated,
        )
    } else {
        Vec::new()
    };
    let links = if request.capture.links {
        string_array(doc.get("links"))
            .into_iter()
            .filter_map(|raw| allowed_output_url(&raw, allowed))
            .collect()
    } else {
        Vec::new()
    };
    let mut media = Vec::new();
    if request.capture.media {
        for (position, raw) in normalized_media(doc.get("envoyageMedia"), doc.get("images"))
            .into_iter()
            .enumerate()
        {
            if *returned_assets >= request.limits.max_assets {
                truncated = true;
                break;
            }
            let Some(url) = allowed_output_url(&raw.url, allowed_assets) else {
                continue;
            };
            media.push(CrawlMedia {
                id: sha256_text(&url),
                position: raw.position.unwrap_or(position as u32),
                url,
                alt: raw.alt,
                width: raw.width,
                height: raw.height,
            });
            *returned_assets += 1;
        }
    }
    Ok(Some(CrawlPage {
        url: url.to_string(),
        canonical_url: metadata_string(meta, "canonicalUrl")
            .and_then(|raw| allowed_output_url(&raw, allowed)),
        page_type: metadata_string(meta, "pageType"),
        product_key: metadata_string(meta, "productKey"),
        breadcrumbs: string_array(doc.get("breadcrumbs"))
            .into_iter()
            .take(20)
            .collect(),
        title: metadata_string(meta, "title"),
        description: metadata_string(meta, "description"),
        status_code: meta
            .and_then(|m| m.get("statusCode"))
            .and_then(Value::as_u64)
            .and_then(|v| u16::try_from(v).ok()),
        sections,
        links,
        media,
        markdown,
        html,
        content_sha256,
        truncated,
    }))
}

fn allowed_output_url(raw: &str, allowed: &BTreeSet<String>) -> Option<String> {
    let url = Url::parse(raw).ok()?;
    let host = url.host_str()?.to_ascii_lowercase();
    (matches!(url.scheme(), "http" | "https") && allowed.contains(&host)).then(|| url.to_string())
}

fn metadata_string(meta: Option<&serde_json::Map<String, Value>>, key: &str) -> Option<String> {
    meta.and_then(|m| m.get(key))
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

fn string_array(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect()
}

struct NormalizedMedia {
    url: String,
    position: Option<u32>,
    alt: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
}

fn normalized_media(structured: Option<&Value>, plain: Option<&Value>) -> Vec<NormalizedMedia> {
    if let Some(rows) = structured.and_then(Value::as_array) {
        return rows
            .iter()
            .filter_map(|row| {
                let url = row.get("url")?.as_str()?.to_string();
                Some(NormalizedMedia {
                    url,
                    position: row
                        .get("position")
                        .and_then(Value::as_u64)
                        .and_then(|value| u32::try_from(value).ok()),
                    alt: row.get("alt").and_then(Value::as_str).map(str::to_string),
                    width: row
                        .get("width")
                        .and_then(Value::as_u64)
                        .and_then(|value| u32::try_from(value).ok()),
                    height: row
                        .get("height")
                        .and_then(Value::as_u64)
                        .and_then(|value| u32::try_from(value).ok()),
                })
            })
            .collect();
    }
    string_array(plain)
        .into_iter()
        .map(|url| NormalizedMedia {
            url,
            position: None,
            alt: None,
            width: None,
            height: None,
        })
        .collect()
}

fn take_content(raw: &str, used: &mut u64, max: u64, truncated: &mut bool) -> Option<String> {
    if raw.is_empty() || *used >= max {
        if !raw.is_empty() {
            *truncated = true;
        }
        return None;
    }
    let remaining = usize::try_from(max - *used).unwrap_or(usize::MAX);
    let end = floor_char_boundary(raw, remaining.min(raw.len()));
    if end < raw.len() {
        *truncated = true;
    }
    *used += end as u64;
    Some(raw[..end].to_string())
}

fn markdown_sections(
    raw: &str,
    used: &mut u64,
    max: u64,
    truncated: &mut bool,
) -> Vec<CrawlSection> {
    let mut sections = Vec::new();
    let mut current_level = 1_u8;
    let mut current_heading = String::new();
    let mut body = String::new();
    let flush = |sections: &mut Vec<CrawlSection>,
                 level: u8,
                 heading: &mut String,
                 body: &mut String,
                 used: &mut u64,
                 truncated: &mut bool| {
        if heading.is_empty() && body.trim().is_empty() {
            return;
        }
        if *used >= max {
            *truncated = true;
            return;
        }
        let text = body.trim().to_string();
        let wanted = heading.len() + text.len();
        if *used + wanted as u64 > max {
            *truncated = true;
            return;
        }
        *used += wanted as u64;
        sections.push(CrawlSection {
            level,
            heading: std::mem::take(heading),
            text,
        });
        body.clear();
    };
    for line in raw.lines() {
        let hashes = line.bytes().take_while(|b| *b == b'#').count();
        if (1..=6).contains(&hashes) && line.as_bytes().get(hashes) == Some(&b' ') {
            flush(
                &mut sections,
                current_level,
                &mut current_heading,
                &mut body,
                used,
                truncated,
            );
            current_level = hashes as u8;
            current_heading = line[hashes + 1..].trim().to_string();
        } else {
            body.push_str(line);
            body.push('\n');
        }
    }
    flush(
        &mut sections,
        current_level,
        &mut current_heading,
        &mut body,
        used,
        truncated,
    );
    sections
}

fn floor_char_boundary(s: &str, mut index: usize) -> usize {
    while index > 0 && !s.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn empty_job(receipt: &Receipt, state: CrawlState) -> CrawlJob {
    CrawlJob {
        id: receipt.id.clone(),
        adapter: receipt.adapter.clone(),
        adapter_version: receipt.adapter_version.clone(),
        state,
        request_fingerprint: receipt.request_fingerprint.clone(),
        created_at_ms: receipt.created_at_ms,
        progress: CrawlProgress {
            completed_pages: 0,
            total_pages: 0,
            returned_pages: 0,
            returned_assets: 0,
            returned_content_bytes: 0,
        },
        pages: Vec::new(),
        next_cursor: None,
        warnings: Vec::new(),
        error: None,
    }
}

fn validate_idempotency_key(key: &str) -> Result<(), String> {
    if !(8..=200).contains(&key.len()) || !key.bytes().all(|b| b.is_ascii_graphic()) {
        return Err("Idempotency-Key must contain 8 to 200 visible ASCII characters".to_string());
    }
    Ok(())
}

fn validate_job_id(id: &str) -> Result<(), String> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err("invalid crawl job id".to_string());
    }
    Ok(())
}

fn validate_asset_id(id: &str) -> Result<(), String> {
    if id.len() != 64 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("invalid crawl asset id".to_string());
    }
    Ok(())
}

fn envoyage_job_id(provider_id: &str, request_fingerprint: &str, created_at_ms: u64) -> String {
    let digest = sha256_text(&format!(
        "envoyage-crawl\0{provider_id}\0{request_fingerprint}\0{created_at_ms}"
    ));
    format!("crawl-{}", &digest[..32])
}

fn encode_cursor(next: &str) -> String {
    URL_SAFE_NO_PAD.encode(next.as_bytes())
}

fn decode_cursor(cursor: &str) -> Result<String, String> {
    let bytes = URL_SAFE_NO_PAD
        .decode(cursor)
        .map_err(|_| "invalid crawl cursor".to_string())?;
    String::from_utf8(bytes).map_err(|_| "invalid crawl cursor".to_string())
}

fn sha256_json<T: Serialize>(value: &T) -> Result<String, String> {
    let bytes = serde_json::to_vec(value).map_err(|e| format!("serialize crawl request: {e}"))?;
    Ok(sha256_bytes(&bytes))
}

fn sha256_text(value: &str) -> String {
    sha256_bytes(value.as_bytes())
}

fn sha256_bytes(value: &[u8]) -> String {
    let digest = Sha256::digest(value);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn download_public_image(
    raw_url: &str,
    allowed_hosts: &BTreeSet<String>,
    max_bytes: u64,
) -> Result<CrawlAssetDownload, String> {
    let mut current = Url::parse(raw_url).map_err(|e| format!("invalid crawl asset URL: {e}"))?;
    for redirect_count in 0..=MAX_ASSET_REDIRECTS {
        let normalized = validate_public_url(current.as_str(), false)?;
        let host = normalized
            .host_str()
            .ok_or("crawl asset URL must have a host")?
            .to_ascii_lowercase();
        if !allowed_hosts.contains(&host) {
            return Err("crawl asset redirect left allowedHosts".to_string());
        }
        let port = normalized
            .port_or_known_default()
            .ok_or("crawl asset URL uses an unsupported port")?;
        let address = resolve_public_address(&host, port)?;
        let client = Client::builder()
            .timeout(Duration::from_secs(60))
            .redirect(reqwest::redirect::Policy::none())
            .resolve(&host, address)
            .build()
            .map_err(|e| format!("build crawl asset client: {e}"))?;
        let response = client
            .get(normalized.clone())
            .send()
            .map_err(|e| format!("download crawl asset: {e}"))?;
        if response.status().is_redirection() {
            if redirect_count == MAX_ASSET_REDIRECTS {
                return Err("crawl asset exceeded redirect limit".to_string());
            }
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or("crawl asset redirect has no valid Location")?;
            current = normalized
                .join(location)
                .map_err(|e| format!("invalid crawl asset redirect: {e}"))?;
            continue;
        }
        if !response.status().is_success() {
            return Err(format!(
                "crawl asset download failed: HTTP {}",
                response.status().as_u16()
            ));
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .map(str::trim)
            .unwrap_or_default()
            .to_ascii_lowercase();
        if !supported_raster_content_type(&content_type) {
            return Err("crawl asset is not a supported raster image".to_string());
        }
        if response
            .content_length()
            .is_some_and(|length| length > max_bytes)
        {
            return Err("crawl asset exceeds its byte limit".to_string());
        }
        let mut bytes = Vec::new();
        response
            .take(max_bytes.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|e| format!("read crawl asset: {e}"))?;
        if bytes.len() as u64 > max_bytes {
            return Err("crawl asset exceeds its byte limit".to_string());
        }
        return Ok(CrawlAssetDownload {
            content_type,
            sha256: sha256_bytes(&bytes),
            bytes,
        });
    }
    Err("crawl asset exceeded redirect limit".to_string())
}

fn supported_raster_content_type(content_type: &str) -> bool {
    matches!(
        content_type,
        "image/jpeg"
            | "image/png"
            | "image/webp"
            | "image/avif"
            | "image/gif"
            | "image/heic"
            | "image/heif"
            | "image/tiff"
    )
}

fn resolve_public_address(host: &str, port: u16) -> Result<SocketAddr, String> {
    let addresses = (host, port)
        .to_socket_addrs()
        .map_err(|e| format!("resolve crawl asset host {host}: {e}"))?
        .collect::<Vec<_>>();
    if addresses.is_empty() {
        return Err(format!("crawl asset host did not resolve: {host}"));
    }
    if addresses.iter().any(|address| !is_public_ip(address.ip())) {
        return Err(format!(
            "crawl asset host resolves to a private or reserved address: {host}"
        ));
    }
    Ok(addresses[0])
}

fn read_json<T: DeserializeOwned>(path: &Path, label: &str) -> Result<T, String> {
    let bytes = fs::read(path).map_err(|e| format!("read {label}: {e}"))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("decode {label}: {e}"))
}

fn write_json<T: Serialize>(path: &Path, value: &T, label: &str) -> Result<(), String> {
    let bytes = serde_json::to_vec(value).map_err(|e| format!("serialize {label}: {e}"))?;
    write_bytes_atomic(path, &bytes, label)
}

fn write_bytes_atomic(path: &Path, bytes: &[u8], label: &str) -> Result<(), String> {
    let tmp = path.with_extension(format!("tmp-{}-{}", std::process::id(), now_ms()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .map_err(|e| format!("create {label}: {e}"))?;
    file.write_all(bytes)
        .map_err(|e| format!("write {label}: {e}"))?;
    file.sync_all().map_err(|e| format!("sync {label}: {e}"))?;
    fs::rename(&tmp, path).map_err(|e| format!("publish {label}: {e}"))
}

fn append_json_line<T: Serialize>(path: &Path, value: &T, label: &str) -> Result<(), String> {
    let mut bytes = serde_json::to_vec(value).map_err(|e| format!("serialize {label}: {e}"))?;
    bytes.push(b'\n');
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("open {label}: {e}"))?;
    file.write_all(&bytes)
        .map_err(|e| format!("append {label}: {e}"))?;
    file.sync_data().map_err(|e| format!("sync {label}: {e}"))
}

fn write_receipt(path: &Path, receipt: &Receipt) -> Result<(), String> {
    let bytes = serde_json::to_vec(receipt).map_err(|e| format!("serialize crawl receipt: {e}"))?;
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .map_err(|e| format!("create crawl receipt: {e}"))?;
    file.write_all(&bytes)
        .map_err(|e| format!("write crawl receipt: {e}"))?;
    file.sync_all()
        .map_err(|e| format!("sync crawl receipt: {e}"))?;
    fs::rename(&tmp, path).map_err(|e| format!("publish crawl receipt: {e}"))
}

fn read_receipt(path: &Path) -> Result<Receipt, String> {
    let bytes = fs::read(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            "crawl job not found".to_string()
        } else {
            format!("read crawl receipt: {e}")
        }
    })?;
    serde_json::from_slice(&bytes).map_err(|e| format!("decode crawl receipt: {e}"))
}

fn envoyage_home() -> PathBuf {
    std::env::var_os("ENVOYAGE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".envoyage")))
        .unwrap_or_else(|| PathBuf::from(".envoyage"))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn u32_value(value: Option<&Value>) -> u32 {
    value
        .and_then(Value::as_u64)
        .and_then(|v| u32::try_from(v).ok())
        .unwrap_or(0)
}

fn provider_error(status: u16, value: &Value) -> String {
    let message = value
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or("crawl provider error");
    format!("crawl provider HTTP {status}: {message}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct MockProvider {
        starts: AtomicUsize,
        request: Mutex<Option<CrawlRequest>>,
    }

    impl MockProvider {
        fn new() -> Self {
            Self {
                starts: AtomicUsize::new(0),
                request: Mutex::new(None),
            }
        }
    }

    impl CrawlProvider for MockProvider {
        fn start(
            &self,
            request: &CrawlRequest,
            _idempotency_key: &str,
        ) -> Result<ProviderStart, String> {
            self.starts.fetch_add(1, Ordering::SeqCst);
            *self.request.lock().unwrap() = Some(request.clone());
            Ok(ProviderStart {
                id: "crawl-123".to_string(),
                adapter: "generic".to_string(),
                adapter_version: crawl_adapter_version(),
                asset_hosts: request.allowed_hosts.clone(),
            })
        }

        fn read(&self, _provider_id: &str, _next: Option<&str>) -> Result<ProviderStatus, String> {
            Ok(ProviderStatus {
                state: CrawlState::Completed,
                completed: 2,
                total: 2,
                documents: vec![json!({
                    "markdown": "# Product\nSoft cotton set.",
                    "links": ["https://shop.example/products/one", "https://evil.example/track"],
                    "images": ["https://shop.example/a.jpg", "https://evil.example/pixel.gif"],
                    "metadata": {
                        "sourceURL": "https://shop.example/products/one",
                        "title": "One",
                        "statusCode": 200
                    }
                })],
                next: Some("https://provider.example/v2/crawl/crawl-123?cursor=next".to_string()),
                warning: None,
                error: None,
            })
        }

        fn cancel(&self, _provider_id: &str) -> Result<(), String> {
            Ok(())
        }

        fn validate_cursor(&self, provider_id: &str, next: &str) -> bool {
            provider_id == "crawl-123"
                && next.starts_with("https://provider.example/v2/crawl/crawl-123")
        }
    }

    fn request() -> CrawlRequest {
        CrawlRequest {
            url: "https://shop.example/collections/summer".to_string(),
            adapter: CrawlAdapter::Generic,
            allowed_hosts: vec!["shop.example".to_string()],
            include_paths: vec!["products/.*".to_string()],
            exclude_paths: vec![],
            discovery: CrawlDiscovery::SitemapAndLinks,
            render: CrawlRenderPolicy::Auto,
            capture: CrawlCapture::default(),
            limits: CrawlLimits::default(),
        }
    }

    fn test_dir(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("envoyage-crawl-{label}-{}", now_ms()));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn rejects_private_network_targets_and_unsafe_limits() {
        let mut req = request();
        req.url = "http://127.0.0.1/admin".to_string();
        assert!(
            validate_request(req, false)
                .unwrap_err()
                .contains("private")
        );

        let mut req = request();
        req.limits.max_pages = MAX_PAGE_LIMIT + 1;
        assert!(
            validate_request(req, false)
                .unwrap_err()
                .contains("maxPages")
        );

        let mut req = request();
        req.limits.max_content_bytes = MAX_CONTENT_BYTES;
        assert_eq!(
            validate_request(req, false).unwrap().limits.max_content_bytes,
            1024 * 1024 * 1024
        );

        let mut req = request();
        req.limits.max_content_bytes = MAX_CONTENT_BYTES + 1;
        assert!(
            validate_request(req, false)
                .unwrap_err()
                .contains("maxContentBytes")
        );
    }

    #[test]
    fn exact_idempotency_replays_and_changed_payload_is_rejected() {
        let provider = Arc::new(MockProvider::new());
        let dir = test_dir("idempotency");
        let service = CrawlService::new(provider.clone(), dir.clone(), false).unwrap();
        let first = service.start(request(), "factory-import-001").unwrap();
        let replay = service.start(request(), "factory-import-001").unwrap();
        assert_eq!(first.id, replay.id);
        assert!(first.id.starts_with("crawl-"));
        assert_ne!(first.id, "crawl-123");
        assert_eq!(provider.starts.load(Ordering::SeqCst), 1);
        let audit = fs::read_to_string(service.audit_path(&first.id)).unwrap();
        assert_eq!(audit.lines().count(), 1);
        assert!(audit.contains("\"action\":\"start\""));

        let mut changed = request();
        changed.limits.max_pages = 12;
        assert!(
            service
                .start(changed, "factory-import-001")
                .unwrap_err()
                .contains("different")
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn normalizes_only_allowlisted_pages_links_and_ordered_media() {
        let provider = Arc::new(MockProvider::new());
        let dir = test_dir("normalize");
        let service = CrawlService::new(provider, dir.clone(), false).unwrap();
        let started = service.start(request(), "factory-import-002").unwrap();
        let job = service.read(&started.id, None).unwrap();
        assert_eq!(job.state, CrawlState::Completed);
        assert_eq!(job.pages.len(), 1);
        assert_eq!(
            job.pages[0].links,
            vec!["https://shop.example/products/one"]
        );
        assert_eq!(job.pages[0].media.len(), 1);
        assert_eq!(job.pages[0].media[0].position, 0);
        assert_eq!(job.pages[0].sections[0].heading, "Product");
        assert!(job.next_cursor.is_some());
        let manifest: BTreeMap<String, String> =
            read_json(&service.asset_manifest_path(&started.id), "test manifest").unwrap();
        assert_eq!(
            manifest.get(&job.pages[0].media[0].id).map(String::as_str),
            Some("https://shop.example/a.jpg")
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn verified_product_pages_keep_gallery_identity_order_and_dimensions() {
        let request = CrawlRequest {
            adapter: CrawlAdapter::ShopifyCollection,
            ..request()
        };
        let mut bytes = 0;
        let mut assets = 0;
        let mut warnings = Vec::new();
        let page = normalize_document(
            json!({
                "markdown": "Soft cotton set",
                "breadcrumbs": ["Summer", "Black cotton set"],
                "envoyageMedia": [{
                    "url": "https://cdn.shop.example/black-front.jpg",
                    "position": 3,
                    "alt": "front",
                    "width": 2001,
                    "height": 3000
                }],
                "metadata": {
                    "sourceURL": "https://shop.example/products/black-set",
                    "canonicalUrl": "https://shop.example/products/black-set",
                    "title": "Black cotton set",
                    "pageType": "product",
                    "productKey": "black-set",
                    "contentSha256": "a".repeat(64)
                }
            }),
            &request,
            &BTreeSet::from(["shop.example".to_string()]),
            &BTreeSet::from(["cdn.shop.example".to_string()]),
            &mut bytes,
            &mut assets,
            &mut warnings,
        )
        .unwrap()
        .unwrap();
        assert_eq!(page.page_type.as_deref(), Some("product"));
        assert_eq!(page.product_key.as_deref(), Some("black-set"));
        assert_eq!(page.breadcrumbs, vec!["Summer", "Black cotton set"]);
        assert_eq!(page.media[0].position, 3);
        assert_eq!(page.media[0].width, Some(2001));
        assert_eq!(page.media[0].height, Some(3000));
        assert_eq!(page.content_sha256, "a".repeat(64));
    }

    #[test]
    fn empty_arrays_remain_present_in_the_wire_contract() {
        let provider = Arc::new(MockProvider::new());
        let dir = test_dir("wire-arrays");
        let service = CrawlService::new(provider, dir.clone(), false).unwrap();
        let job = service.start(request(), "factory-import-003").unwrap();
        let value = serde_json::to_value(job).unwrap();
        assert_eq!(value.get("pages"), Some(&json!([])));
        assert_eq!(value.get("warnings"), Some(&json!([])));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn asset_ids_and_content_types_are_strict() {
        assert!(validate_asset_id(&"a".repeat(64)).is_ok());
        assert!(validate_asset_id("../secret").is_err());
        assert!(supported_raster_content_type("image/jpeg"));
        assert!(supported_raster_content_type("image/heic"));
        assert!(!supported_raster_content_type("image/svg+xml"));
        assert!(!supported_raster_content_type("text/html"));
    }

    #[test]
    fn cursor_is_opaque_and_bound_to_the_job() {
        let raw = "https://provider.example/v2/crawl/crawl-123?cursor=next";
        let encoded = encode_cursor(raw);
        assert!(!encoded.contains("provider.example"));
        assert_eq!(decode_cursor(&encoded).unwrap(), raw);
        assert!(decode_cursor("not base64!!").is_err());
    }

    #[test]
    fn content_budget_is_visible_not_silent() {
        let request = CrawlRequest {
            limits: CrawlLimits {
                max_content_bytes: 8,
                ..CrawlLimits::default()
            },
            capture: CrawlCapture {
                markdown: true,
                sections: false,
                ..CrawlCapture::default()
            },
            ..request()
        };
        let mut bytes = 0;
        let mut assets = 0;
        let mut warnings = Vec::new();
        let page = normalize_document(
            json!({"markdown":"1234567890", "metadata":{"sourceURL":"https://shop.example/x"}}),
            &request,
            &BTreeSet::from(["shop.example".to_string()]),
            &BTreeSet::from(["shop.example".to_string()]),
            &mut bytes,
            &mut assets,
            &mut warnings,
        )
        .unwrap()
        .unwrap();
        assert_eq!(page.markdown.as_deref(), Some("12345678"));
        assert!(page.truncated);
        assert_eq!(bytes, 8);
    }
}
