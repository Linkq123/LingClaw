import type { AppState } from './state.js';
import { syncAllExecutionStackVisibility } from './renderers/execution-stack.js';

export function applyToolsVisibility(
  showTools: boolean,
  deps: {
    state: Pick<AppState, 'showTools' | 'activeToolPanel'>;
    chat: HTMLElement | null;
    closeToolDrawer: () => void;
    closeSubagentModal: () => void;
    closeOrchestrateTaskModal: () => void;
  },
) {
  deps.state.showTools = showTools;
  deps.chat?.classList.toggle('hide-tools', !showTools);

  if (!showTools) {
    deps.closeToolDrawer();
    deps.closeSubagentModal();
    deps.closeOrchestrateTaskModal();
    deps.state.activeToolPanel = null;
  }

  // Modal hosts are restored from placeholders by the close handlers above;
  // apply filtering after that restoration so the real execution step, not
  // only its temporary clone, receives the hidden state.
  syncAllExecutionStackVisibility();
}
