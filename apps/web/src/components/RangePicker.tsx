"use client";

import { DatePicker, type DateRange } from "@cloudflare/kumo/components/date-picker";

// Split out so PlanToolbar can lazy-load it: the calendar (react-day-picker
// + date-fns) is most of Kumo's weight and only matters once the popover opens.
export default function RangePicker({
  selected,
  onChange,
}: {
  selected: DateRange | undefined;
  onChange: (range: DateRange | undefined) => void;
}) {
  return <DatePicker mode="range" timeZone="UTC" max={365} selected={selected} onChange={onChange} />;
}
