//! Frozen format-3 wire contract for RelationWindow consumers.
//!
//! The format-2 handler remains the only active runtime until the backend
//! migration change switches every consumer together. These types deliberately
//! have no server registration or fallback decoder.
//!
//! Proposal-3 activation checklist: migrate `rich_relations_command`,
//! `RichRelationResponse`, `CompactRelationDto`, and cursor handling in
//! `server/call_hierarchy.rs`; migrate `normalizeRichRelationResponse` and the
//! request sites in `callRelationsView.ts`; update `tests/lsp_smoke.rs`; then
//! delete the format-2 DTO, decoder, and dictionary-ID normalization path.

use std::collections::HashSet;
use std::fmt;

use serde::{de, Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

pub const RELATION_WINDOW_PROTOCOL_VERSION: u32 = 3;
pub const RELATION_REQUEST_MAX_BYTES: usize = 32 * 1024;
pub const RELATION_RESPONSE_MAX_BYTES: usize = 1024 * 1024;
pub const RELATION_PAGE_SIZE_MAX: u16 = 200;
pub const RELATION_PAGE_ITEMS_MAX: usize = 200;
pub const RELATION_PAGE_NODES_MAX: usize = 400;
pub const RELATION_PREPARE_ROOTS_MAX: usize = 200;

const REF_MAX_CODE_POINTS: usize = 4096;
const URI_MAX_CODE_POINTS: usize = 8192;
const NAME_MAX_CODE_POINTS: usize = 512;
const QUALIFIED_NAME_MAX_CODE_POINTS: usize = 1024;
const DETAIL_MAX_CODE_POINTS: usize = 2048;
const EXCERPT_MAX_CODE_POINTS: usize = 4096;
const REASON_MAX_CODE_POINTS: usize = 512;
const REASON_LIST_MAX: usize = 32;
const JAVASCRIPT_SAFE_INTEGER_MAX: u64 = 9_007_199_254_740_991;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Nullable<T>(pub Option<T>);

impl<T: Serialize> Serialize for Nullable<T> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.0.serialize(serializer)
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Nullable<T> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Option::<T>::deserialize(deserializer).map(Self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DecimalU64(pub u64);

impl Serialize for DecimalU64 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0.to_string())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct SafeUnsignedInteger(pub u64);

impl<'de> Deserialize<'de> for SafeUnsignedInteger {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = u64::deserialize(deserializer)?;
        if value > JAVASCRIPT_SAFE_INTEGER_MAX {
            Err(de::Error::custom(
                "integer exceeds the JavaScript safe integer range",
            ))
        } else {
            Ok(Self(value))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct SafeInteger(pub i64);

impl<'de> Deserialize<'de> for SafeInteger {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = i64::deserialize(deserializer)?;
        if value.unsigned_abs() > JAVASCRIPT_SAFE_INTEGER_MAX {
            Err(de::Error::custom(
                "integer exceeds the JavaScript safe integer range",
            ))
        } else {
            Ok(Self(value))
        }
    }
}

impl<'de> Deserialize<'de> for DecimalU64 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        if value.is_empty()
            || !value.bytes().all(|byte| byte.is_ascii_digit())
            || (value.len() > 1 && value.starts_with('0'))
        {
            return Err(de::Error::custom("expected a canonical decimal u64 string"));
        }
        value
            .parse::<u64>()
            .map(Self)
            .map_err(|_| de::Error::custom("decimal u64 string is out of range"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationFamily {
    CCpp,
    Go,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RelationSnapshot {
    pub service_instance: String,
    pub database_identity: String,
    pub workspace_uri: String,
    pub family: RelationFamily,
    pub engine_epoch: DecimalU64,
    pub semantic_generation: DecimalU64,
    pub overlay_epoch: DecimalU64,
    pub resolver_version: SafeUnsignedInteger,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RelationPosition {
    pub line: SafeUnsignedInteger,
    pub character: SafeUnsignedInteger,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RelationRange {
    pub start: RelationPosition,
    pub end: RelationPosition,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RelationLocation {
    pub uri: String,
    pub range: RelationRange,
    pub document_version: Nullable<SafeInteger>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum RelationRootInput {
    Symbol {
        uri: String,
        position: RelationPosition,
        document_version: Nullable<SafeInteger>,
    },
    File {
        uri: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RelationKind {
    Calls,
    References,
    Members,
    Inherits,
    TypeOf,
    Includes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationNodeKind {
    Callable,
    Record,
    Field,
    Variable,
    Local,
    Alias,
    Macro,
    File,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationSupport {
    Supported,
    Partial,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RelationCapability {
    pub relation: RelationKind,
    pub direction: super::RelationDirection,
    pub support: RelationSupport,
    pub reason: Nullable<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RelationNode {
    #[serde(rename = "ref")]
    pub reference: String,
    pub kind: RelationNodeKind,
    pub name: String,
    pub qualified_name: String,
    pub detail: Nullable<String>,
    pub display_truncated: bool,
    pub relations: Vec<RelationCapability>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationResolution {
    Resolved,
    Candidate,
    Unresolved,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationRole {
    Read,
    Write,
    ReadWrite,
    Call,
    TypeUse,
    Address,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KnownSiteCount {
    pub value: SafeUnsignedInteger,
    pub exact: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RelationEdge {
    #[serde(rename = "ref")]
    pub reference: String,
    pub source_ref: String,
    pub target_ref: Nullable<String>,
    pub relation: RelationKind,
    pub resolution: RelationResolution,
    pub ambiguity_set_ref: Nullable<String>,
    pub roles: Vec<RelationRole>,
    pub known_site_count: KnownSiteCount,
    pub reason_codes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RelationEvidenceItem {
    #[serde(rename = "ref")]
    pub reference: String,
    pub location: Nullable<RelationLocation>,
    pub excerpt: Nullable<String>,
    pub excerpt_truncated: bool,
    pub supports: Vec<String>,
    pub contradictions: Vec<String>,
    pub unknowns: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationCoverageState {
    Complete,
    Partial,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RelationCoverage {
    pub state: RelationCoverageState,
    pub scope: String,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RelationContinuation {
    End,
    Page { cursor: String },
    Limited { reason: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationBudget {
    Within,
    ScanLimit,
    CandidateLimit,
    ResponseLimit,
    Deadline,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RelationPageMeta {
    pub continuation: RelationContinuation,
    pub coverage: RelationCoverage,
    pub budget: RelationBudget,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PrepareRelationParams {
    pub root: RelationRootInput,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExpandRelationParams {
    pub snapshot: RelationSnapshot,
    pub node_ref: String,
    pub relation: RelationKind,
    pub direction: super::RelationDirection,
    pub cursor: Nullable<String>,
    pub page_size: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvidenceRelationParams {
    pub snapshot: RelationSnapshot,
    pub edge_ref: String,
    pub cursor: Nullable<String>,
    pub page_size: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationLocateTargetKind {
    Node,
    Evidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RelationLocateTarget {
    pub kind: RelationLocateTargetKind,
    #[serde(rename = "ref")]
    pub reference: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationLocateIntent {
    Definition,
    Declaration,
    Site,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LocateRelationParams {
    pub snapshot: RelationSnapshot,
    pub target: RelationLocateTarget,
    pub intent: RelationLocateIntent,
    pub cursor: Nullable<String>,
    pub page_size: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "operation",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum RelationRequest {
    Prepare {
        protocol_version: u32,
        params: PrepareRelationParams,
    },
    Expand {
        protocol_version: u32,
        params: ExpandRelationParams,
    },
    Evidence {
        protocol_version: u32,
        params: EvidenceRelationParams,
    },
    Locate {
        protocol_version: u32,
        params: LocateRelationParams,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PrepareRelationData {
    pub snapshot: RelationSnapshot,
    pub roots: Vec<RelationNode>,
    pub selection_required: bool,
    pub coverage: RelationCoverage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExpandRelationData {
    pub snapshot: RelationSnapshot,
    pub nodes: Vec<RelationNode>,
    pub edges: Vec<RelationEdge>,
    pub page: RelationPageMeta,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvidenceRelationData {
    pub snapshot: RelationSnapshot,
    pub items: Vec<RelationEvidenceItem>,
    pub page: RelationPageMeta,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LocateRelationData {
    pub snapshot: RelationSnapshot,
    pub locations: Vec<RelationLocation>,
    pub page: RelationPageMeta,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationErrorCode {
    VersionMismatch,
    InvalidRequest,
    InvalidReference,
    InvalidCursor,
    StaleSnapshot,
    SnapshotUnavailable,
    Unsupported,
    Cancelled,
    ResourceLimit,
    ExecutionFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RelationWireError {
    pub code: RelationErrorCode,
    pub message: String,
    pub retryable: bool,
    pub reason_codes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum RelationOutcome<T> {
    Ok { data: T },
    Error { error: RelationWireError },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelationResponse<T> {
    pub protocol_version: u32,
    #[serde(flatten)]
    pub outcome: RelationOutcome<T>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodedRelationResponse {
    Prepare(RelationResponse<PrepareRelationData>),
    Expand(RelationResponse<ExpandRelationData>),
    Evidence(RelationResponse<EvidenceRelationData>),
    Locate(RelationResponse<LocateRelationData>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelationOperation {
    Prepare,
    Expand,
    Evidence,
    Locate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelationContractError {
    pub code: RelationErrorCode,
    pub message: String,
}

impl RelationContractError {
    fn invalid(message: impl Into<String>) -> Self {
        Self {
            code: RelationErrorCode::InvalidRequest,
            message: message.into(),
        }
    }

    fn version(version: u32) -> Self {
        Self {
            code: RelationErrorCode::VersionMismatch,
            message: format!(
                "unsupported relation protocol {version}; expected {RELATION_WINDOW_PROTOCOL_VERSION}"
            ),
        }
    }
}

impl fmt::Display for RelationContractError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for RelationContractError {}

impl RelationErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::VersionMismatch => "version_mismatch",
            Self::InvalidRequest => "invalid_request",
            Self::InvalidReference => "invalid_reference",
            Self::InvalidCursor => "invalid_cursor",
            Self::StaleSnapshot => "stale_snapshot",
            Self::SnapshotUnavailable => "snapshot_unavailable",
            Self::Unsupported => "unsupported",
            Self::Cancelled => "cancelled",
            Self::ResourceLimit => "resource_limit",
            Self::ExecutionFailed => "execution_failed",
        }
    }
}

pub fn decode_relation_request(json: &str) -> Result<RelationRequest, RelationContractError> {
    if json.len() > RELATION_REQUEST_MAX_BYTES {
        return Err(RelationContractError::invalid("request exceeds 32 KiB"));
    }
    let value: serde_json::Value = serde_json::from_str(json)
        .map_err(|error| RelationContractError::invalid(error.to_string()))?;
    validate_request_shape(&value)?;
    let request: RelationRequest = serde_json::from_value(value)
        .map_err(|error| RelationContractError::invalid(error.to_string()))?;
    request.validate()?;
    Ok(request)
}

pub fn decode_relation_response(
    operation: RelationOperation,
    json: &str,
) -> Result<DecodedRelationResponse, RelationContractError> {
    if json.len() > RELATION_RESPONSE_MAX_BYTES {
        return Err(RelationContractError::invalid("response exceeds 1 MiB"));
    }
    let value: serde_json::Value = serde_json::from_str(json)
        .map_err(|error| RelationContractError::invalid(error.to_string()))?;
    validate_response_shape(operation, &value)?;
    macro_rules! decode {
        ($data:ty, $variant:ident) => {{
            let response: RelationResponse<$data> = serde_json::from_value(value)
                .map_err(|error| RelationContractError::invalid(error.to_string()))?;
            response.validate()?;
            Ok(DecodedRelationResponse::$variant(response))
        }};
    }
    match operation {
        RelationOperation::Prepare => decode!(PrepareRelationData, Prepare),
        RelationOperation::Expand => decode!(ExpandRelationData, Expand),
        RelationOperation::Evidence => decode!(EvidenceRelationData, Evidence),
        RelationOperation::Locate => decode!(LocateRelationData, Locate),
    }
}

impl RelationRequest {
    pub fn protocol_version(&self) -> u32 {
        match self {
            Self::Prepare {
                protocol_version, ..
            }
            | Self::Expand {
                protocol_version, ..
            }
            | Self::Evidence {
                protocol_version, ..
            }
            | Self::Locate {
                protocol_version, ..
            } => *protocol_version,
        }
    }

    pub fn validate(&self) -> Result<(), RelationContractError> {
        if self.protocol_version() != RELATION_WINDOW_PROTOCOL_VERSION {
            return Err(RelationContractError::version(self.protocol_version()));
        }
        match self {
            Self::Prepare { params, .. } => params.validate(),
            Self::Expand { params, .. } => params.validate(),
            Self::Evidence { params, .. } => params.validate(),
            Self::Locate { params, .. } => params.validate(),
        }
    }
}

impl<T: RelationDataValidation> RelationResponse<T> {
    fn validate(&self) -> Result<(), RelationContractError> {
        if self.protocol_version != RELATION_WINDOW_PROTOCOL_VERSION {
            return Err(RelationContractError::version(self.protocol_version));
        }
        match &self.outcome {
            RelationOutcome::Ok { data } => data.validate(),
            RelationOutcome::Error { error } => error.validate(),
        }
    }
}

trait RelationDataValidation {
    fn validate(&self) -> Result<(), RelationContractError>;
}

impl PrepareRelationParams {
    fn validate(&self) -> Result<(), RelationContractError> {
        match &self.root {
            RelationRootInput::Symbol { uri, .. } | RelationRootInput::File { uri } => {
                validate_uri(uri)
            }
        }
    }
}

impl ExpandRelationParams {
    fn validate(&self) -> Result<(), RelationContractError> {
        self.snapshot.validate()?;
        validate_ref(&self.node_ref)?;
        validate_cursor(&self.cursor)?;
        validate_page_size(self.page_size)
    }
}

impl EvidenceRelationParams {
    fn validate(&self) -> Result<(), RelationContractError> {
        self.snapshot.validate()?;
        validate_ref(&self.edge_ref)?;
        validate_cursor(&self.cursor)?;
        validate_page_size(self.page_size)
    }
}

impl LocateRelationParams {
    fn validate(&self) -> Result<(), RelationContractError> {
        self.snapshot.validate()?;
        validate_ref(&self.target.reference)?;
        validate_cursor(&self.cursor)?;
        validate_page_size(self.page_size)?;
        let valid_pair = matches!(
            (self.target.kind, self.intent),
            (
                RelationLocateTargetKind::Node,
                RelationLocateIntent::Definition | RelationLocateIntent::Declaration
            ) | (
                RelationLocateTargetKind::Evidence,
                RelationLocateIntent::Site
            )
        );
        if valid_pair {
            Ok(())
        } else {
            Err(RelationContractError::invalid(
                "locate target kind and intent are incompatible",
            ))
        }
    }
}

impl RelationDataValidation for PrepareRelationData {
    fn validate(&self) -> Result<(), RelationContractError> {
        self.snapshot.validate()?;
        if self.roots.len() > RELATION_PREPARE_ROOTS_MAX {
            return Err(RelationContractError::invalid("prepare roots exceed 200"));
        }
        for node in &self.roots {
            node.validate()?;
        }
        self.coverage.validate()
    }
}

impl RelationDataValidation for ExpandRelationData {
    fn validate(&self) -> Result<(), RelationContractError> {
        self.snapshot.validate()?;
        if self.nodes.len() > RELATION_PAGE_NODES_MAX || self.edges.len() > RELATION_PAGE_ITEMS_MAX
        {
            return Err(RelationContractError::invalid(
                "expand page exceeds node or edge limits",
            ));
        }
        let mut node_refs = HashSet::with_capacity(self.nodes.len());
        for node in &self.nodes {
            node.validate()?;
            if !node_refs.insert(node.reference.as_str()) {
                return Err(RelationContractError::invalid("duplicate node ref in page"));
            }
        }
        for edge in &self.edges {
            edge.validate()?;
            if !node_refs.contains(edge.source_ref.as_str())
                || edge
                    .target_ref
                    .0
                    .as_deref()
                    .is_some_and(|reference| !node_refs.contains(reference))
            {
                return Err(RelationContractError {
                    code: RelationErrorCode::InvalidReference,
                    message: "edge endpoint is absent from the page node dictionary".into(),
                });
            }
        }
        self.page.validate()
    }
}

impl RelationDataValidation for EvidenceRelationData {
    fn validate(&self) -> Result<(), RelationContractError> {
        self.snapshot.validate()?;
        if self.items.len() > RELATION_PAGE_ITEMS_MAX {
            return Err(RelationContractError::invalid(
                "evidence page exceeds 200 items",
            ));
        }
        for item in &self.items {
            item.validate()?;
        }
        self.page.validate()
    }
}

impl RelationDataValidation for LocateRelationData {
    fn validate(&self) -> Result<(), RelationContractError> {
        self.snapshot.validate()?;
        if self.locations.len() > RELATION_PAGE_ITEMS_MAX {
            return Err(RelationContractError::invalid(
                "location page exceeds 200 items",
            ));
        }
        for location in &self.locations {
            location.validate()?;
        }
        self.page.validate()
    }
}

impl RelationSnapshot {
    fn validate(&self) -> Result<(), RelationContractError> {
        validate_nonempty_limited(
            "serviceInstance",
            &self.service_instance,
            REF_MAX_CODE_POINTS,
        )?;
        validate_nonempty_limited(
            "databaseIdentity",
            &self.database_identity,
            REF_MAX_CODE_POINTS,
        )?;
        validate_uri(&self.workspace_uri)
    }
}

impl RelationLocation {
    fn validate(&self) -> Result<(), RelationContractError> {
        validate_uri(&self.uri)?;
        if (self.range.end.line, self.range.end.character)
            < (self.range.start.line, self.range.start.character)
        {
            return Err(RelationContractError::invalid(
                "range end precedes range start",
            ));
        }
        Ok(())
    }
}

impl RelationNode {
    fn validate(&self) -> Result<(), RelationContractError> {
        validate_ref(&self.reference)?;
        validate_nonempty_limited("name", &self.name, NAME_MAX_CODE_POINTS)?;
        validate_limited(
            "qualifiedName",
            &self.qualified_name,
            QUALIFIED_NAME_MAX_CODE_POINTS,
        )?;
        if let Some(detail) = &self.detail.0 {
            validate_limited("detail", detail, DETAIL_MAX_CODE_POINTS)?;
        }
        for capability in &self.relations {
            if let Some(reason) = &capability.reason.0 {
                validate_limited("capability reason", reason, REASON_MAX_CODE_POINTS)?;
            }
        }
        Ok(())
    }
}

impl RelationEdge {
    fn validate(&self) -> Result<(), RelationContractError> {
        validate_ref(&self.reference)?;
        validate_ref(&self.source_ref)?;
        match (self.resolution, &self.target_ref.0) {
            (RelationResolution::Unresolved, None)
            | (RelationResolution::Resolved | RelationResolution::Candidate, Some(_)) => {}
            _ => {
                return Err(RelationContractError::invalid(
                    "targetRef does not match edge resolution",
                ));
            }
        }
        if let Some(reference) = &self.target_ref.0 {
            validate_ref(reference)?;
        }
        if let Some(reference) = &self.ambiguity_set_ref.0 {
            validate_ref(reference)?;
        }
        validate_reason_list("reasonCodes", &self.reason_codes)
    }
}

impl RelationEvidenceItem {
    fn validate(&self) -> Result<(), RelationContractError> {
        validate_ref(&self.reference)?;
        if let Some(location) = &self.location.0 {
            location.validate()?;
        }
        if let Some(excerpt) = &self.excerpt.0 {
            validate_limited("excerpt", excerpt, EXCERPT_MAX_CODE_POINTS)?;
        }
        validate_reason_list("supports", &self.supports)?;
        validate_reason_list("contradictions", &self.contradictions)?;
        validate_reason_list("unknowns", &self.unknowns)
    }
}

impl RelationCoverage {
    fn validate(&self) -> Result<(), RelationContractError> {
        validate_limited("coverage scope", &self.scope, REASON_MAX_CODE_POINTS)?;
        validate_reason_list("coverage reasons", &self.reasons)
    }
}

impl RelationPageMeta {
    fn validate(&self) -> Result<(), RelationContractError> {
        match &self.continuation {
            RelationContinuation::End => {}
            RelationContinuation::Page { cursor } => validate_ref(cursor)?,
            RelationContinuation::Limited { reason } => {
                validate_nonempty_limited("limited reason", reason, REASON_MAX_CODE_POINTS)?
            }
        }
        self.coverage.validate()
    }
}

impl RelationWireError {
    fn validate(&self) -> Result<(), RelationContractError> {
        validate_nonempty_limited("error message", &self.message, REASON_MAX_CODE_POINTS)?;
        validate_reason_list("error reasonCodes", &self.reason_codes)
    }
}

pub fn merge_known_site_count(
    current: KnownSiteCount,
    incoming: KnownSiteCount,
) -> Result<KnownSiteCount, RelationContractError> {
    if current.exact && incoming.exact && current.value != incoming.value {
        return Err(RelationContractError::invalid(
            "conflicting exact knownSiteCount values",
        ));
    }
    if current.exact && !incoming.exact && incoming.value > current.value
        || incoming.exact && !current.exact && current.value > incoming.value
    {
        return Err(RelationContractError::invalid(
            "knownSiteCount lower bound exceeds exact value",
        ));
    }
    if current.exact {
        return Ok(current);
    }
    if incoming.exact {
        return Ok(incoming);
    }
    Ok(KnownSiteCount {
        value: current.value.max(incoming.value),
        exact: false,
    })
}

pub fn same_relation_snapshot(left: &RelationSnapshot, right: &RelationSnapshot) -> bool {
    left == right
}

fn validate_request_shape(value: &Value) -> Result<(), RelationContractError> {
    let request = value_object(value, "request")?;
    exact_object_keys(
        request,
        &["protocolVersion", "operation", "params"],
        "request",
    )?;
    validate_wire_version(request.get("protocolVersion"))?;
    let operation = request
        .get("operation")
        .and_then(Value::as_str)
        .ok_or_else(|| RelationContractError::invalid("request.operation must be a string"))?;
    let params = value_object(
        request
            .get("params")
            .ok_or_else(|| RelationContractError::invalid("request is missing params"))?,
        "request.params",
    )?;
    match operation {
        "prepare" => {
            let root = value_object(
                params.get("root").ok_or_else(|| {
                    RelationContractError::invalid("prepare.params is missing root")
                })?,
                "prepare.params.root",
            )?;
            if root.get("kind").and_then(Value::as_str) == Some("symbol") {
                require_field(root, "documentVersion", "prepare.params.root")?;
            }
        }
        "expand" | "evidence" | "locate" => {
            require_field(params, "cursor", &format!("{operation}.params"))?;
        }
        _ => {}
    }
    Ok(())
}

fn validate_response_shape(
    operation: RelationOperation,
    value: &Value,
) -> Result<(), RelationContractError> {
    let response = value_object(value, "response")?;
    let outcome = response
        .get("outcome")
        .and_then(Value::as_str)
        .ok_or_else(|| RelationContractError::invalid("response.outcome must be a string"))?;
    match outcome {
        "ok" => exact_object_keys(
            response,
            &["protocolVersion", "outcome", "data"],
            "response",
        )?,
        "error" => exact_object_keys(
            response,
            &["protocolVersion", "outcome", "error"],
            "response",
        )?,
        _ => return Ok(()),
    }
    validate_wire_version(response.get("protocolVersion"))?;
    if outcome == "error" {
        return Ok(());
    }
    let data = value_object(
        response
            .get("data")
            .ok_or_else(|| RelationContractError::invalid("response is missing data"))?,
        "response.data",
    )?;
    match operation {
        RelationOperation::Prepare => {
            for node in value_array_field(data, "roots", "response.data")? {
                validate_node_nullable_shape(node)?;
            }
        }
        RelationOperation::Expand => {
            for node in value_array_field(data, "nodes", "response.data")? {
                validate_node_nullable_shape(node)?;
            }
            for edge in value_array_field(data, "edges", "response.data")? {
                let edge = value_object(edge, "response.data.edges[]")?;
                require_field(edge, "targetRef", "response.data.edges[]")?;
                require_field(edge, "ambiguitySetRef", "response.data.edges[]")?;
            }
        }
        RelationOperation::Evidence => {
            for item in value_array_field(data, "items", "response.data")? {
                let item = value_object(item, "response.data.items[]")?;
                require_field(item, "location", "response.data.items[]")?;
                require_field(item, "excerpt", "response.data.items[]")?;
                if let Some(location) = item.get("location").filter(|value| !value.is_null()) {
                    validate_location_nullable_shape(location)?;
                }
            }
        }
        RelationOperation::Locate => {
            for location in value_array_field(data, "locations", "response.data")? {
                validate_location_nullable_shape(location)?;
            }
        }
    }
    Ok(())
}

fn validate_node_nullable_shape(value: &Value) -> Result<(), RelationContractError> {
    let node = value_object(value, "relation node")?;
    require_field(node, "detail", "relation node")?;
    for capability in value_array_field(node, "relations", "relation node")? {
        require_field(
            value_object(capability, "relation capability")?,
            "reason",
            "relation capability",
        )?;
    }
    Ok(())
}

fn validate_location_nullable_shape(value: &Value) -> Result<(), RelationContractError> {
    require_field(
        value_object(value, "relation location")?,
        "documentVersion",
        "relation location",
    )
}

fn validate_wire_version(value: Option<&Value>) -> Result<(), RelationContractError> {
    let Some(version) = value.and_then(Value::as_u64) else {
        return Err(RelationContractError::invalid(
            "protocolVersion must be a non-negative integer",
        ));
    };
    let version = u32::try_from(version)
        .map_err(|_| RelationContractError::invalid("protocolVersion exceeds u32"))?;
    if version == RELATION_WINDOW_PROTOCOL_VERSION {
        Ok(())
    } else {
        Err(RelationContractError::version(version))
    }
}

fn value_object<'a>(
    value: &'a Value,
    path: &str,
) -> Result<&'a serde_json::Map<String, Value>, RelationContractError> {
    value
        .as_object()
        .ok_or_else(|| RelationContractError::invalid(format!("{path} must be an object")))
}

fn value_array_field<'a>(
    object: &'a serde_json::Map<String, Value>,
    field: &str,
    path: &str,
) -> Result<&'a Vec<Value>, RelationContractError> {
    object
        .get(field)
        .and_then(Value::as_array)
        .ok_or_else(|| RelationContractError::invalid(format!("{path}.{field} must be an array")))
}

fn require_field(
    object: &serde_json::Map<String, Value>,
    field: &str,
    path: &str,
) -> Result<(), RelationContractError> {
    if object.contains_key(field) {
        Ok(())
    } else {
        Err(RelationContractError::invalid(format!(
            "{path} is missing {field}"
        )))
    }
}

fn exact_object_keys(
    object: &serde_json::Map<String, Value>,
    expected: &[&str],
    path: &str,
) -> Result<(), RelationContractError> {
    for key in object.keys() {
        if !expected.contains(&key.as_str()) {
            return Err(RelationContractError::invalid(format!(
                "{path} contains unknown field {key}"
            )));
        }
    }
    for key in expected {
        require_field(object, key, path)?;
    }
    Ok(())
}

fn validate_page_size(page_size: u16) -> Result<(), RelationContractError> {
    if (1..=RELATION_PAGE_SIZE_MAX).contains(&page_size) {
        Ok(())
    } else {
        Err(RelationContractError::invalid(
            "pageSize must be between 1 and 200",
        ))
    }
}

fn validate_cursor(cursor: &Nullable<String>) -> Result<(), RelationContractError> {
    if let Some(cursor) = &cursor.0 {
        validate_ref(cursor)?;
    }
    Ok(())
}

fn validate_ref(reference: &str) -> Result<(), RelationContractError> {
    validate_nonempty_limited("ref", reference, REF_MAX_CODE_POINTS)
}

fn validate_uri(uri: &str) -> Result<(), RelationContractError> {
    validate_nonempty_limited("uri", uri, URI_MAX_CODE_POINTS)
}

fn validate_reason_list(field: &str, values: &[String]) -> Result<(), RelationContractError> {
    if values.len() > REASON_LIST_MAX {
        return Err(RelationContractError::invalid(format!(
            "{field} exceeds 32 entries"
        )));
    }
    for value in values {
        validate_limited(field, value, REASON_MAX_CODE_POINTS)?;
    }
    Ok(())
}

fn validate_nonempty_limited(
    field: &str,
    value: &str,
    maximum: usize,
) -> Result<(), RelationContractError> {
    if value.is_empty() {
        return Err(RelationContractError::invalid(format!(
            "{field} must not be empty"
        )));
    }
    validate_limited(field, value, maximum)
}

fn validate_limited(field: &str, value: &str, maximum: usize) -> Result<(), RelationContractError> {
    if value.chars().count() > maximum {
        Err(RelationContractError::invalid(format!(
            "{field} exceeds {maximum} Unicode code points"
        )))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;
    use serde_json::Value;

    use super::*;

    const VALID_FIXTURES: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/relations/valid.json"
    ));
    const INVALID_FIXTURES: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/relations/invalid.json"
    ));
    const SCENARIO_FIXTURES: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/relations/scenarios.json"
    ));

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct WireFixtureSuite {
        contract_version: u32,
        cases: Vec<WireFixture>,
        #[serde(default)]
        generated_cases: Vec<GeneratedFixture>,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct WireFixture {
        name: String,
        #[serde(default)]
        producer: Option<String>,
        kind: String,
        operation: String,
        #[serde(default)]
        expected_code: Option<String>,
        value: Value,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct GeneratedFixture {
        name: String,
        kind: String,
        operation: String,
        expected_code: String,
        repeat_code_point: String,
        repeat_count: usize,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct ScenarioSuite {
        contract_version: u32,
        count_merges: Vec<CountMergeFixture>,
        snapshot_comparisons: Vec<SnapshotFixture>,
        cursor_rejections: Vec<CursorFixture>,
        terminal_races: Vec<TerminalFixture>,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct CountMergeFixture {
        name: String,
        current: KnownSiteCount,
        incoming: KnownSiteCount,
        #[serde(default)]
        expected: Option<KnownSiteCount>,
        #[serde(default)]
        expected_code: Option<String>,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct SnapshotFixture {
        name: String,
        left: RelationSnapshot,
        right: RelationSnapshot,
        expected: bool,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct CursorFixture {
        name: String,
        changed_binding: String,
        expected_code: String,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct TerminalFixture {
        name: String,
        first: String,
        late: String,
        accepted_terminal_count: u32,
    }

    #[test]
    fn relation_contract_shared_valid_fixtures_round_trip_through_rust() {
        let suite: WireFixtureSuite = serde_json::from_str(VALID_FIXTURES).unwrap();
        assert_eq!(suite.contract_version, RELATION_WINDOW_PROTOCOL_VERSION);
        let mut rust_count = 0;
        let mut typescript_count = 0;
        for fixture in suite.cases {
            match fixture.producer.as_deref() {
                Some("rust") => rust_count += 1,
                Some("typescript") => typescript_count += 1,
                other => panic!("{} has invalid producer {other:?}", fixture.name),
            }
            let json = serde_json::to_string(&fixture.value).unwrap();
            let encoded = if fixture.kind == "request" {
                serde_json::to_value(
                    decode_relation_request(&json)
                        .unwrap_or_else(|error| panic!("{}: {error}", fixture.name)),
                )
                .unwrap()
            } else {
                let response = decode_relation_response(operation(&fixture.operation), &json)
                    .unwrap_or_else(|error| panic!("{}: {error}", fixture.name));
                response_value(response)
            };
            assert_eq!(
                encoded, fixture.value,
                "{} changed on round trip",
                fixture.name
            );
        }
        assert!(rust_count > 0 && typescript_count > 0);
    }

    #[test]
    fn relation_contract_shared_invalid_fixtures_are_rejected_by_rust() {
        let suite: WireFixtureSuite = serde_json::from_str(INVALID_FIXTURES).unwrap();
        assert_eq!(suite.contract_version, RELATION_WINDOW_PROTOCOL_VERSION);
        for fixture in suite.cases {
            let json = serde_json::to_string(&fixture.value).unwrap();
            let error = if fixture.kind == "request" {
                decode_relation_request(&json).unwrap_err()
            } else {
                decode_relation_response(operation(&fixture.operation), &json).unwrap_err()
            };
            assert_eq!(
                Some(error.code.as_str()),
                fixture.expected_code.as_deref(),
                "{} returned {error}",
                fixture.name
            );
        }
        for fixture in suite.generated_cases {
            let repeated = fixture.repeat_code_point.repeat(fixture.repeat_count);
            let json = if fixture.kind == "request" {
                serde_json::json!({
                    "protocolVersion": 3,
                    "operation": "prepare",
                    "params": { "root": { "kind": "file", "uri": repeated } }
                })
                .to_string()
            } else {
                serde_json::json!({
                    "protocolVersion": 3,
                    "outcome": "error",
                    "error": { "code": "resource_limit", "message": repeated, "retryable": false, "reasonCodes": [] }
                })
                .to_string()
            };
            let error = if fixture.kind == "request" {
                decode_relation_request(&json).unwrap_err()
            } else {
                decode_relation_response(operation(&fixture.operation), &json).unwrap_err()
            };
            assert_eq!(
                error.code.as_str(),
                fixture.expected_code,
                "{}",
                fixture.name
            );
        }
    }

    #[test]
    fn relation_contract_shared_state_scenarios_are_monotonic_and_snapshot_bound() {
        let suite: ScenarioSuite = serde_json::from_str(SCENARIO_FIXTURES).unwrap();
        assert_eq!(suite.contract_version, RELATION_WINDOW_PROTOCOL_VERSION);
        for fixture in suite.count_merges {
            match (
                merge_known_site_count(fixture.current, fixture.incoming),
                fixture.expected,
            ) {
                (Ok(actual), Some(expected)) => assert_eq!(actual, expected, "{}", fixture.name),
                (Err(error), None) => assert_eq!(
                    Some(error.code.as_str()),
                    fixture.expected_code.as_deref(),
                    "{}",
                    fixture.name
                ),
                (actual, expected) => {
                    panic!("{}: unexpected {actual:?} / {expected:?}", fixture.name)
                }
            }
        }
        for fixture in suite.snapshot_comparisons {
            assert_eq!(
                same_relation_snapshot(&fixture.left, &fixture.right),
                fixture.expected,
                "{}",
                fixture.name
            );
        }
        let allowed_bindings = ["nodeRef", "direction", "operation", "pageSize"];
        for fixture in suite.cursor_rejections {
            assert!(
                allowed_bindings.contains(&fixture.changed_binding.as_str()),
                "{}",
                fixture.name
            );
            assert_eq!(fixture.expected_code, "invalid_cursor", "{}", fixture.name);
        }
        for fixture in suite.terminal_races {
            assert_ne!(fixture.first, fixture.late, "{}", fixture.name);
            assert_eq!(fixture.accepted_terminal_count, 1, "{}", fixture.name);
        }
    }

    fn operation(value: &str) -> RelationOperation {
        match value {
            "prepare" => RelationOperation::Prepare,
            "expand" => RelationOperation::Expand,
            "evidence" => RelationOperation::Evidence,
            "locate" => RelationOperation::Locate,
            other => panic!("unknown fixture operation {other}"),
        }
    }

    fn response_value(response: DecodedRelationResponse) -> Value {
        match response {
            DecodedRelationResponse::Prepare(value) => serde_json::to_value(value).unwrap(),
            DecodedRelationResponse::Expand(value) => serde_json::to_value(value).unwrap(),
            DecodedRelationResponse::Evidence(value) => serde_json::to_value(value).unwrap(),
            DecodedRelationResponse::Locate(value) => serde_json::to_value(value).unwrap(),
        }
    }
}
