use tower_lsp::jsonrpc::Result as LspResult;
use tower_lsp::lsp_types::{SignatureHelp, SignatureHelpParams};

use super::Backend;

impl Backend {
    pub(super) async fn provide_signature_help(
        &self,
        _params: SignatureHelpParams,
    ) -> LspResult<Option<SignatureHelp>> {
        Ok(None)
    }
}
