import Graph from "../../components/Graph";

export default async function Widget({
  searchParams,
}: {
  searchParams: Promise<{ theme?: string; api?: string }>;
}) {
  const { theme, api } = await searchParams;
  return <Graph theme={theme === "dark" ? "dark" : "light"} apiParam={api} />;
}