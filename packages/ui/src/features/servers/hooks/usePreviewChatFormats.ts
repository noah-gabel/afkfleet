// Trying unsaved chat formats on a line (Plan.md P11.9; ADR-0020, U58, R148): the server says
// which sender the first format that fits reads, and which format it was, counted from 0.
// Nothing is stored or recorded, and no step-up is needed. A request holds formats and a line
// from a server's chat, never a secret, so this is a mutation.
import { useMutation } from "@tanstack/react-query";

import { useApiClient } from "../../../api/context";
import type { PreviewChatFormatsRequest } from "../../../generated/PreviewChatFormatsRequest";

/** The preview's mutation; see the module comment. */
export function usePreviewChatFormats() {
  const api = useApiClient();
  return useMutation({
    mutationFn: (request: PreviewChatFormatsRequest) => api.previewChatFormats(request),
  });
}
