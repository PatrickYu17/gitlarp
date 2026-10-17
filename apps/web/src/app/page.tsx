import { Text } from "@cloudflare/kumo/components/text";
import { GitCommitIcon } from "@phosphor-icons/react/dist/ssr";

import Graph from "../components/Graph";

export default function Home() {
  return (
    <main className="mx-auto flex w-full max-w-5xl flex-col gap-6 px-4 py-8 sm:px-6">
      <header className="flex items-center gap-3">
        <span className="flex size-10 items-center justify-center rounded-xl bg-kumo-brand text-white">
          <GitCommitIcon size={22} weight="bold" />
        </span>
        <div className="flex flex-col">
          <Text variant="heading" size="lg" as="h1">
            gitlarp
          </Text>
          <Text variant="secondary" size="sm">
            Plan backdated commits on your GitHub contribution graph.
          </Text>
        </div>
      </header>
      <Graph tools />
    </main>
  );
}
