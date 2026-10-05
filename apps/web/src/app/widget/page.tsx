import Graph from "../../components/Graph";

// `?api=` is deliberately ignored: the widget is embeddable cross-origin
// (middleware.ts sets `frame-ancestors *` for /widget), so forwarding the param
// would let any site frame this page as `/widget?api=https://attacker.tld`
// and receive the visitor's PAT as a Bearer header. Graph resolves its
// base from sessionStorage → build-time env → default instead, and shows
// the resolved destination next to the PAT input.
export default async function Widget({
  searchParams,
}: {
  searchParams: Promise<{ theme?: string }>;
}) {
  const { theme } = await searchParams;
  return <Graph theme={theme === "dark" ? "dark" : "light"} />;
}
