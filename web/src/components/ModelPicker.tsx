import { useAsrStatus, useSetAsrModel } from "../api/hooks";

export function ModelPicker() {
  const status = useAsrStatus();
  const setModel = useSetAsrModel();

  if (!status.data) {
    return <div className="model-picker">ASR: loading…</div>;
  }

  return (
    <label className="model-picker">
      <span>ASR model</span>
      <select
        value={status.data.model}
        disabled={setModel.isPending}
        onChange={(event) => setModel.mutate({ model: event.target.value })}
      >
        {status.data.models.map((model) => (
          <option key={model.name} value={model.name}>
            {model.label}
            {model.installed ? " ✓" : ""}
          </option>
        ))}
      </select>
    </label>
  );
}
