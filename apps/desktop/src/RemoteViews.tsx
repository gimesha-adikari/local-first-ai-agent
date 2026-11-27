import { CloudIcon, CpuIcon, MonitorIcon } from "./icons";
import {
  type ProviderView, formatTokens, listText, localityLabel, retentionText, sharingSummary, usedTokens,
} from "./providers";

/** What the next Chat request will share, shown with the composer before sending. */
export function RemoteDisclosure({ provider, onOpen }: { provider: ProviderView; onOpen: () => void }) {
  const { shared, withheld } = sharingSummary(provider.settings.sharing);
  const limit = provider.settings.budget.monthlyTokens;
  const left = limit === null ? null : Math.max(0, limit - usedTokens(provider.usage));
  return (
    <section className="remote-disclosure" aria-label="Remote provider disclosure">
      <CloudIcon size={15} />
      <div>
        <p>
          <strong>{provider.settings.name}</strong> · {provider.settings.model} · {localityLabel(provider)}
        </p>
        <p className="hint">
          Sends {listText(shared)}{withheld.length ? `; never ${listText(withheld)}` : ""}. {retentionText(provider.settings)}
          {" "}{left === null ? "No monthly limit." : `${formatTokens(left)} tokens left this month (estimated).`}
        </p>
      </div>
      <button type="button" className="link-button" onClick={onOpen}>Change</button>
    </section>
  );
}

/** First run without a local model or provider: choose how Chat should work. */
export function Onboarding({ onLocal, onProviders }: { onLocal: () => void; onProviders: () => void }) {
  return (
    <section className="onboarding" aria-label="Get started">
      <h3>Choose how Chat works</h3>
      <p className="hint">The app has no built-in model. Pick one way to start; you can add the others later.</p>
      <ul className="onboarding-options">
        <li>
          <button type="button" className="onboarding-option" onClick={onLocal}>
            <CpuIcon size={18} />
            <span><strong>Local model</strong><small>Discover a verified download or import a GGUF file. Local inference runs offline.</small></span>
          </button>
        </li>
        <li>
          <button type="button" className="onboarding-option" onClick={onProviders}>
            <CloudIcon size={18} />
            <span><strong>Cloud provider</strong><small>An OpenAI-compatible service. You choose what may be sent and set a monthly limit.</small></span>
          </button>
        </li>
        <li>
          <button type="button" className="onboarding-option" onClick={onProviders}>
            <MonitorIcon size={18} />
            <span><strong>Self-hosted server</strong><small>A server you run on this computer or your network, such as llama.cpp or Ollama.</small></span>
          </button>
        </li>
      </ul>
    </section>
  );
}
