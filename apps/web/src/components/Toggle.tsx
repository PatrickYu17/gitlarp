import type { ReactNode } from "react";

/**
 * A native checkbox in Kumo colours. Kumo's Checkbox pulls in Base UI's
 * checkbox (~4 KB gzipped), which the first-load JS budget can't absorb.
 */
export default function Toggle({
  label,
  title,
  checked,
  onChange,
}: {
  label: ReactNode;
  title?: string;
  checked: boolean;
  onChange: (checked: boolean) => void;
}) {
  return (
    <label className="flex w-fit cursor-pointer items-center gap-2 text-sm text-kumo-default" title={title}>
      <input
        type="checkbox"
        className="m-0 size-4 cursor-pointer accent-kumo-brand"
        checked={checked}
        onChange={(e) => onChange(e.target.checked)}
      />
      {label}
    </label>
  );
}
