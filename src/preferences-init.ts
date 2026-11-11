// Independent of the app bundle so saved appearance applies before the UI mounts.
import "./theme";
import "./language";
import bundledStyles from "./design-system.css?inline";
import { startDesignSystemUpdates } from "./design-system";

const stopStyleUpdates = startDesignSystemUpdates(bundledStyles);
if (import.meta.hot) import.meta.hot.dispose(stopStyleUpdates);
