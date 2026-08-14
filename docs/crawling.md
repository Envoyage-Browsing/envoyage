# Bounded website crawling

Envoyage can inventory a public website, return normalized page structure and
media, and download only the raster images found by that crawl. Consumers use
one Envoyage contract even when the configured crawl engine changes.

For a public Shopify collection, `adapter: "auto"` uses the built-in verified
collection adapter. It returns one Product page per Product with its stable
handle, canonical URL and complete ordered image gallery. It snapshots the
feed before returning, so a later website change cannot alter an existing job.
Other sites use the configured generic crawl provider.

Use crawling for a bounded site inventory. Use the existing `browser_*` tools
when an agent must understand one rendered page in context or interact with it.

## Configure the engine

`envoyage serve` reads these server-only values:

| Variable | Purpose |
| --- | --- |
| `ENVOYAGE_CRAWL_PROVIDER_URL` | Base URL of an unmodified Firecrawl v2 deployment. |
| `ENVOYAGE_CRAWL_PROVIDER_TOKEN` | Optional provider bearer token. Never returned to callers. |
| `ENVOYAGE_CRAWL_STATE_DIR` | Durable receipts, media manifests and bounded downloaded bytes. Defaults to `${ENVOYAGE_HOME}/crawls`. |

The ordinary `ENVOYAGE_AUTH_TOKEN` protects the HTTP and Streamable HTTP MCP
surfaces. Provider credentials are not accepted in requests or MCP arguments.

## Start, read and cancel

```sh
curl -sS -X POST "$ENVOYAGE_URL/crawls" \
  -H "Authorization: Bearer $ENVOYAGE_TOKEN" \
  -H "Idempotency-Key: summer-catalogue-v1" \
  -H "Content-Type: application/json" \
  --data '{
    "url":"https://shop.example/collections/summer",
    "adapter":"auto",
    "allowedHosts":["shop.example"],
    "includePaths":["products/.*"],
    "render":"auto",
    "capture":{"sections":true,"links":true,"media":true},
    "limits":{"maxPages":250,"maxAssets":2000,"maxContentBytes":67108864}
  }'
```

Read the returned job with `GET /crawls/{id}`. If `nextCursor` is present,
pass it unchanged as `?cursor=...`. It is opaque and valid only for that job.
Cancel with `DELETE /crawls/{id}`.

Each media item has a stable `id`. After reading the result page that contains
it, download the exact bytes from `GET /crawls/{id}/assets/{assetId}`. Envoyage
re-checks the allowed host and public DNS address at every redirect, accepts
only bounded raster images, caches the first successful bytes and returns their
SHA-256 in `X-Envoyage-Content-Sha256`.

## SDK

```ts
import { createCrawlClient } from "@envoyage/browser";

const crawl = createCrawlClient({
  endpoint: process.env.ENVOYAGE_URL!,
  token: process.env.ENVOYAGE_TOKEN,
});

let job = await crawl.start(
  {
    url: "https://shop.example/collections/summer",
    adapter: "auto",
    allowedHosts: ["shop.example"],
    capture: { sections: true, links: true, media: true },
    limits: { maxPages: 250, maxAssets: 2000 },
  },
  "summer-catalogue-v1",
);

job = await crawl.read(job.id);
const first = job.pages[0]?.media[0];
if (first) {
  const image = await crawl.downloadAsset(job.id, first.id);
  console.log(image.contentType, image.sha256, image.bytes.byteLength);
}
```

## MCP

The same domain service is available over stdio and Streamable HTTP MCP:

- `crawl_start` — accepts an `idempotency_key` and bounded `request`.
- `crawl_read` — accepts Envoyage job ID and optional opaque cursor.
- `crawl_cancel` — cancels one exact job.

Media bytes use the authenticated HTTP asset route because MCP JSON should not
carry large binary files.

## Limits and safety

- Only `http` and `https` seed URLs are accepted. Credentials, fragments,
  localhost, private/reserved addresses and non-standard ports are rejected.
- `allowedHosts` is mandatory after normalization and includes the seed host.
  Returned pages, links, redirects and media are filtered against it.
- DNS is resolved before crawling. Asset downloads pin a checked public address
  for each request and repeat the check after every redirect.
- The hard ceilings are 2,000 pages, 20,000 media items, 250 MiB of returned
  content, one hour, depth 20 and concurrency 20. Requests may choose lower
  limits. Truncation is visible in the result.
- `Idempotency-Key` replay returns the original job only when the normalized
  request is identical. Reusing the key with changed input fails.
- `render: auto` is the normal path. `render: browser` asks the configured
  provider to wait for rendered page content. The first Firecrawl adapter
  rejects `render: static` because it cannot honestly guarantee a static-only
  fetch.
- Envoyage respects provider robots handling and does not expose a control that
  bypasses it. Consumers remain responsible for permission, copyright and their
  own retention rules.
- The verified Shopify adapter discovers the first-party CDN hosts from the
  collection snapshot, checks their public DNS and permits only the exact image
  URLs listed in that job. Callers do not need to guess or approve a broad CDN
  host, and Envoyage never crawls the CDN as pages.

Firecrawl is an optional engine behind this contract. It is not part of the
Envoyage public API, and its job IDs, cursors, response shape and credentials do
not leave Envoyage.
