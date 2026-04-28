import type { AppState } from './state.js';

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
}
