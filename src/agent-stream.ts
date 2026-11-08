import type { StreamFn } from "@earendil-works/pi-agent-core";
import { anthropicMessagesApi } from "@earendil-works/pi-ai/api/anthropic-messages.lazy";
import { googleGenerativeAIApi } from "@earendil-works/pi-ai/api/google-generative-ai.lazy";
import { openAICompletionsApi } from "@earendil-works/pi-ai/api/openai-completions.lazy";
import { openAIResponsesApi } from "@earendil-works/pi-ai/api/openai-responses.lazy";

/** Dispatch by API so custom provider IDs and native credential resolution keep working. */
export const streamAgent: StreamFn = (model, context, options) => {
  switch (model.api) {
    case "anthropic-messages":
      return anthropicMessagesApi().streamSimple(model, context, options);
    case "google-generative-ai":
      return googleGenerativeAIApi().streamSimple(model, context, options);
    case "openai-completions":
      return openAICompletionsApi().streamSimple(model, context, options);
    case "openai-responses":
      return openAIResponsesApi().streamSimple(model, context, options);
    default:
      throw new Error(`Unsupported provider API: ${model.api}`);
  }
};
