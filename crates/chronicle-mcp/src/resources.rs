use rmcp::model::Resource;

pub const STATUS_URI: &str = "chronicle://status/v1";
const EVENT_SCHEMA_URI: &str = "chronicle://schemas/event/v1";
const CHUNK_SCHEMA_URI: &str = "chronicle://schemas/chunk/v1";
const ARTIFACT_SCHEMA_URI: &str = "chronicle://schemas/derived-artifact/v1";
const QUERY_SCHEMA_URI: &str = "chronicle://schemas/query/v1";

pub fn list() -> Vec<Resource> {
    vec![
        Resource::new(STATUS_URI, "chronicle-status")
            .with_title("Open Chronicle status")
            .with_description("Grant-bounded recording and projection status as structured JSON.")
            .with_mime_type("application/json"),
        schema_resource(
            EVENT_SCHEMA_URI,
            "event-v1",
            "Factual evidence event contract",
        ),
        schema_resource(
            CHUNK_SCHEMA_URI,
            "chunk-v1",
            "Five-minute factual chunk contract",
        ),
        schema_resource(
            ARTIFACT_SCHEMA_URI,
            "derived-artifact-v1",
            "Separate derived analysis contract",
        ),
        schema_resource(QUERY_SCHEMA_URI, "query-v1", "Grant-bounded query contract"),
    ]
}

pub fn static_text(uri: &str) -> Option<(&'static str, &'static str)> {
    match uri {
        EVENT_SCHEMA_URI => Some((
            include_str!("../../../docs/contracts/event-v1.md"),
            "text/markdown",
        )),
        CHUNK_SCHEMA_URI => Some((
            include_str!("../../../docs/contracts/chunk-v1.md"),
            "text/markdown",
        )),
        ARTIFACT_SCHEMA_URI => Some((
            include_str!("../../../docs/contracts/derived-artifact-v1.md"),
            "text/markdown",
        )),
        QUERY_SCHEMA_URI => Some((
            include_str!("../../../docs/contracts/query-v1.md"),
            "text/markdown",
        )),
        _ => None,
    }
}

fn schema_resource(uri: &'static str, name: &'static str, description: &'static str) -> Resource {
    Resource::new(uri, name)
        .with_title(format!("Open Chronicle {name}"))
        .with_description(description)
        .with_mime_type("text/markdown")
}
