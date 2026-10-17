import Graph from "../../components/Graph";

// `?api=` is deliberately ignored: the widget is embeddable cross-origin
// (middleware.ts sets `frame-ancestors *` for /widget), so forwarding the param
// would let any site frame this page as `/widget?api=https://attacker.tld`
// and receive the visitor's PAT as a Bearer header. Graph resolves its
// base from sessionStorage → build-time env → default instead, and shows
// the resolved destination next to the PAT input.
//
// `data-mode` here makes the server HTML right without JS; layout.tsx's
// script mirrors it onto <html> so portalled popups match.
export default async function Widget({
  searchParams,
}: {
  searchParams: Promise<{ theme?: string }>;
}) {
  const { theme } = await searchParams;
  return (
    <div data-mode={theme === "dark" ? "dark" : "light"} className="min-h-screen bg-kumo-canvas p-4 text-kumo-default">
      <Graph />
    </div>
  );
}
