"use client";

import { Banner } from "@cloudflare/kumo/components/banner";
import { Input } from "@cloudflare/kumo/components/input";
import { LayerCard } from "@cloudflare/kumo/components/layer-card";
import { SensitiveInput } from "@cloudflare/kumo/components/sensitive-input";
import { KeyIcon, WarningIcon } from "@phosphor-icons/react/dist/ssr";

import { DEFAULT_API_BASE, isInsecureBase } from "../lib/api-base";
import type { GraphState } from "./useGraphState";

export default function ConnectionPanel({
  pat,
  setPat,
  apiInput,
  setApiInput,
  apiBase,
}: Pick<GraphState, "pat" | "setPat" | "apiInput" | "setApiInput" | "apiBase">) {
  const envBase = process.env.NEXT_PUBLIC_GITLARP_API_URL || DEFAULT_API_BASE;
  return (
    <LayerCard>
      <LayerCard.Secondary className="flex items-center gap-2 font-medium">
        <KeyIcon size={16} className="text-kumo-subtle" />
        Connection
      </LayerCard.Secondary>
      <LayerCard.Primary className="flex flex-col gap-4">
        <SensitiveInput
          label="GitHub PAT"
          labelTooltip="Fine-grained token scoped to the gitlarp-history repo only, with Contents read/write. Kept in this browser tab only."
          placeholder="github_pat_…"
          autoComplete="off"
          value={pat}
          onValueChange={setPat}
          // The destination must always be visible next to the PAT, not
          // only on focus: it is what a user checks before pasting a token.
          description={`PAT will be sent to: ${apiBase}`}
        />
        {isInsecureBase(apiBase) && (
          <Banner
            variant="alert"
            size="sm"
            icon={<WarningIcon weight="fill" />}
            description="Warning: this API base is not HTTPS — your PAT would be sent in cleartext"
          />
        )}
        <Input
          label="API URL"
          type="text"
          placeholder={envBase}
          description="Where your gitlarp API runs. Leave blank for the default."
          value={apiInput}
          onChange={(e) => setApiInput(e.target.value)}
        />
      </LayerCard.Primary>
    </LayerCard>
  );
}
