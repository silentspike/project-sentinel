# Eine neue View auf der Konsolen-Shell bauen (#419)

Die Shell (`src/App.tsx`) rendert drei Säulen (Dashboard · Control-Center · Chat) auf Desktop
und eine `BottomTabBar` auf Mobile. State kommt reaktiv aus dem Store; Live-Daten via WebTransport.

## Schritte
1. **Komponente** unter `src/views/<Name>.tsx` als SolidJS-Funktionskomponente anlegen.
2. **State lesen**: aus `src/stores/console.ts` importieren (`consoleStore`, `status`, `frameCount`).
   Der Store nutzt `createStore` + `reconcile({ key })` — fine-grained reaktiv, kein Voll-Rerender.
3. **Live-Daten**: ein neues Topic im Backend (#431/#432) pushen; in `ingestFrame()` (`stores/console.ts`)
   einen `else if (topic === "<dein-topic>")`-Zweig ergänzen, der den Store via `reconcile` merged.
4. **Controls**: aus `src/components/controls.tsx` wiederverwenden
   (`ProgressBar`, `SearchFilter`, `StatusDropdown`, `LiveIndicator`, `ThemeToggle`, `addToast`).
5. **Große Listen**: `VirtualScroller` (`src/components/VirtualScroller.tsx`) — `rowHeight` + `height` setzen,
   `renderRow` liefern. 10k+ Zeilen flüssig.
6. **Einhängen**: in `App.tsx` in die passende Säule (`DashboardCol`/`ControlCol`/`ChatCol`) einfügen.
   In Phase 2 (#444) wird das feste 3-Spalten-Grid durch die Tiling-Engine ersetzt — Views bleiben gleich,
   nur das Layout-Hosting ändert sich (Panel statt fixe Spalte).

## Konventionen
- Nur CSS-Variablen aus `styles/tokens.css` (kein Hardcode) — Design-Polish ist eine eigene Phase.
- `data-testid` an interaktiven/sichtbaren Elementen (E2E + optische playwright-cli-Verifikation).
- i18n-fähig (deutsche Strings, keine in Logik eingebauten Texte).
- Auth: alle `fetch` mit `credentials: "include"` (httpOnly-Session).
