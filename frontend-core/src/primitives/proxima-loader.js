// proxima-loader.js
// Vanilla JS web component — <proxima-loader>
//
// A planetary-simulation loading state, drawn from the same primitives
// as the Proxima seal. Canvas 2D (not WebGL) — the seal's aesthetic is
// etched/restrained: no glow, no neon, flat fills, dashed orbits. Canvas
// 2D matches that DNA and stays small (~6kb gz) and dependency-free.
//
// Reading:
//   G — ringed planet at centre. The work in progress.
//   F & A — tight binary, fast inner orbit. Facts + Abstractions co-located.
//   P — close companion, slower wider orbit. The active perspective.
//   Etched triangle wakes up between F·A·P each frame, a faint trigon.
//   Outer rule carries the determinate progress arc (or sweeps if indeterminate).
//
// Usage (HTML):
//   <script src="proxima-loader.js"></script>
//   <proxima-loader size="320" theme="dark" label="Loading work surface…"></proxima-loader>
//
// Determinate progress:
//   const el = document.querySelector('proxima-loader');
//   el.progress = 0.42;          // 0..1 ; null/undefined = indeterminate sweep
//   el.label = 'Indexing F…';
//   el.complete();               // graceful settle: orbits decay, ring brightens
//
// Attributes:
//   size      — px (default 320)
//   theme     — "dark" | "light" (default "dark")
//   label     — text below the seal (optional)
//   progress  — 0..1 (optional; absent = indeterminate)
//   speed     — multiplier (default 1)
//   stars     — "on" | "off" (default "on") — faint background starfield
//
// JS API:
//   .progress = number|null
//   .label    = string
//   .complete()                  // play settle animation, fires 'complete' event
//   .reset()                     // back to spinning state
//
// Events:
//   'complete' — fired after settle animation finishes

(function () {
  const PALETTE = {
    dark: {
      bg:        "#0E1013",
      rule:      "#2B313B",
      starF:     "#A8AEBA",
      starA:     "#C9A86A",
      starP:     "#E8E4D6",
      planet:    "#D9C28A",
      planetRim: "#3A3220",
      orbit:     "#3A4150",
      triLine:   "#2B313B",
      label:     "#A8AEBA",
      labelDim:  "#4A505C",
      arc:       "#C9A86A",
      bgStars:   "#3A4150",
    },
    light: {
      bg:        "#F2EEE5",
      rule:      "#C0B8A8",
      starF:     "#5A6070",
      starA:     "#8A6A30",
      starP:     "#1A1812",
      planet:    "#8A6A30",
      planetRim: "#5A4220",
      orbit:     "#C0B8A8",
      triLine:   "#C0B8A8",
      label:     "#3A3528",
      labelDim:  "#9A9080",
      arc:       "#8A6A30",
      bgStars:   "#C0B8A8",
    },
  };

  // Pre-baked starfield positions in unit-square coords (deterministic).
  // Generated with a small LCG so we don't need Math.random at boot.
  function makeStars(n, seed = 1337) {
    let s = seed >>> 0;
    const rand = () => {
      s = (s * 1664525 + 1013904223) >>> 0;
      return s / 0xffffffff;
    };
    const out = [];
    for (let i = 0; i < n; i++) {
      const r2 = rand(); // bias toward edges
      const r = 0.3 + 0.65 * r2;
      const a = rand() * Math.PI * 2;
      out.push({
        x: 50 + Math.cos(a) * r * 47,
        y: 50 + Math.sin(a) * r * 47,
        r: 0.12 + rand() * 0.22,
        tw: rand() * Math.PI * 2, // twinkle phase
        ts: 0.6 + rand() * 1.4,   // twinkle speed
      });
    }
    return out;
  }
  const STARS = makeStars(64);

  class ProximaLoader extends HTMLElement {
    static get observedAttributes() {
      return ["size", "theme", "label", "progress", "speed", "stars"];
    }

    constructor() {
      super();
      this._shadow = this.attachShadow({ mode: "open" });

      this._shadow.innerHTML = `
        <style>
          :host {
            display: inline-block;
            font-family: 'JetBrains Mono', ui-monospace, SFMono-Regular, Menlo, monospace;
            -webkit-font-smoothing: antialiased;
          }
          .wrap {
            display: inline-flex;
            flex-direction: column;
            align-items: center;
            gap: 18px;
          }
          canvas {
            display: block;
            border-radius: 50%;
          }
          .label {
            font-size: 11px;
            letter-spacing: 0.18em;
            text-transform: uppercase;
            color: var(--pl-label, #A8AEBA);
            opacity: 0.85;
            text-align: center;
            min-height: 1em;
            display: flex;
            align-items: center;
            gap: 8px;
          }
          .label .pct {
            color: var(--pl-arc, #C9A86A);
            font-variant-numeric: tabular-nums;
            letter-spacing: 0.08em;
          }
          .label .dot {
            width: 4px; height: 4px;
            background: var(--pl-arc, #C9A86A);
            border-radius: 50%;
            opacity: 0.6;
            animation: blink 1.2s ease-in-out infinite;
          }
          @keyframes blink {
            0%, 100% { opacity: 0.2; }
            50%      { opacity: 0.85; }
          }
          @media (prefers-reduced-motion: reduce) {
            .label .dot { animation: none; opacity: 0.6; }
          }
        </style>
        <div class="wrap" part="wrap">
          <canvas part="canvas"></canvas>
          <div class="label" part="label">
            <span class="text"></span>
            <span class="pct"></span>
            <span class="dot" aria-hidden="true"></span>
          </div>
        </div>
      `;

      this._canvas = this._shadow.querySelector("canvas");
      this._ctx = this._canvas.getContext("2d");
      this._labelEl = this._shadow.querySelector(".label .text");
      this._pctEl = this._shadow.querySelector(".label .pct");
      this._dotEl = this._shadow.querySelector(".label .dot");
      this._labelWrap = this._shadow.querySelector(".label");

      this._size = 320;
      this._theme = "dark";
      this._label = "";
      this._progress = null;     // null = indeterminate
      this._speed = 1;
      this._stars = true;

      this._t0 = performance.now();
      this._last = this._t0;
      this._raf = 0;
      this._running = false;
      this._reduceMotion = matchMedia("(prefers-reduced-motion: reduce)").matches;

      // Settle animation state
      this._settling = false;
      this._settleStart = 0;
      this._settleDur = 1400; // ms
      this._settleAfter = null;

      // Smoothed progress (so the arc doesn't jump)
      this._progSmooth = 0;
    }

    connectedCallback() {
      // Read attrs
      this._readAttrs();
      this._resize();
      this._applyCssVars();
      this._start();
    }

    disconnectedCallback() {
      this._stop();
    }

    attributeChangedCallback(name, _old, val) {
      if (!this.isConnected) return;
      this._readAttrs();
      if (name === "size") this._resize();
      if (name === "theme") this._applyCssVars();
      if (name === "label") this._renderLabel();
      if (name === "progress") this._renderLabel();
    }

    // --- Public API ---
    set progress(v) {
      if (v == null || isNaN(v)) {
        this.removeAttribute("progress");
        this._progress = null;
      } else {
        const clamped = Math.max(0, Math.min(1, +v));
        this._progress = clamped;
        this.setAttribute("progress", String(clamped));
      }
      this._renderLabel();
    }
    get progress() { return this._progress; }

    set label(v) {
      this._label = v == null ? "" : String(v);
      this.setAttribute("label", this._label);
      this._renderLabel();
    }
    get label() { return this._label; }

    complete(opts = {}) {
      if (this._settling) return;
      this._settling = true;
      this._settleStart = performance.now();
      this._settleAfter = opts.then || null;
      // Snap arc to 100% smoothly
      this._progress = 1;
    }

    reset() {
      this._settling = false;
      this._settleAfter = null;
      this._progress = null;
      this.removeAttribute("progress");
      this._t0 = performance.now();
      this._renderLabel();
    }

    // --- Internals ---
    _readAttrs() {
      const size = parseFloat(this.getAttribute("size"));
      if (!isNaN(size) && size > 0) this._size = size;
      const theme = this.getAttribute("theme");
      if (theme === "light" || theme === "dark") this._theme = theme;
      const label = this.getAttribute("label");
      this._label = label || "";
      const p = this.getAttribute("progress");
      this._progress = p == null || p === "" ? null : Math.max(0, Math.min(1, parseFloat(p)));
      const speed = parseFloat(this.getAttribute("speed"));
      if (!isNaN(speed) && speed > 0) this._speed = speed;
      const stars = this.getAttribute("stars");
      this._stars = stars !== "off";
      this._renderLabel();
    }

    _applyCssVars() {
      const p = PALETTE[this._theme];
      this.style.setProperty("--pl-label", p.label);
      this.style.setProperty("--pl-arc", p.arc);
    }

    _resize() {
      const dpr = Math.min(window.devicePixelRatio || 1, 2);
      this._canvas.width = this._size * dpr;
      this._canvas.height = this._size * dpr;
      this._canvas.style.width = this._size + "px";
      this._canvas.style.height = this._size + "px";
      this._dpr = dpr;
    }

    _renderLabel() {
      this._labelEl.textContent = this._label || "";
      if (this._progress != null) {
        this._pctEl.textContent = Math.round(this._progress * 100) + "%";
      } else {
        this._pctEl.textContent = "";
      }
      // Hide whole label row if nothing to show
      const empty = !this._label && this._progress == null;
      this._labelWrap.style.display = empty ? "none" : "flex";
    }

    _start() {
      if (this._running) return;
      this._running = true;
      this._last = performance.now();
      const loop = (now) => {
        if (!this._running) return;
        this._tick(now);
        this._raf = requestAnimationFrame(loop);
      };
      this._raf = requestAnimationFrame(loop);
    }

    _stop() {
      this._running = false;
      cancelAnimationFrame(this._raf);
    }

    _tick(now) {
      const dt = Math.min(50, now - this._last) / 1000;
      this._last = now;

      // Smooth the progress towards the target
      const target = this._progress == null ? 0 : this._progress;
      this._progSmooth += (target - this._progSmooth) * Math.min(1, dt * 4);

      // Settle progress 0..1
      let settle = 0;
      if (this._settling) {
        settle = Math.min(1, (now - this._settleStart) / this._settleDur);
        if (settle >= 1 && this._settleAfter !== "_done") {
          this._settleAfter && this._settleAfter();
          this._settleAfter = "_done";
          this.dispatchEvent(new CustomEvent("complete", { bubbles: true, composed: true }));
        }
      }

      this._draw(now, settle);
    }

    _draw(now, settle) {
      const ctx = this._ctx;
      const dpr = this._dpr;
      const W = this._size, H = this._size;
      ctx.setTransform(1, 0, 0, 1, 0, 0);
      ctx.clearRect(0, 0, this._canvas.width, this._canvas.height);
      ctx.scale(dpr, dpr);

      const p = PALETTE[this._theme];
      const cx = W / 2, cy = H / 2;
      const unit = W / 100; // viewBox-100 → px

      // ---- Background disc ----
      ctx.beginPath();
      ctx.arc(cx, cy, 48 * unit, 0, Math.PI * 2);
      ctx.fillStyle = p.bg;
      ctx.fill();

      // Outer rule
      ctx.beginPath();
      ctx.arc(cx, cy, 47.4 * unit, 0, Math.PI * 2);
      ctx.lineWidth = 0.4 * unit;
      ctx.strokeStyle = p.rule;
      ctx.stroke();

      // ---- Background starfield ----
      if (this._stars) {
        ctx.fillStyle = p.bgStars;
        for (let i = 0; i < STARS.length; i++) {
          const s = STARS[i];
          const tw = 0.45 + 0.55 * (0.5 + 0.5 * Math.sin(now * 0.0009 * s.ts + s.tw));
          ctx.globalAlpha = 0.18 + 0.55 * tw * (1 - settle * 0.6);
          ctx.beginPath();
          ctx.arc(s.x * unit, s.y * unit, s.r * unit, 0, Math.PI * 2);
          ctx.fill();
        }
        ctx.globalAlpha = 1;
      }

      // ---- Time / orbit phases ----
      // Astronomically faithful (compressed in time, scaled in space):
      //   F + A     = α Cen A & B — tight binary at the system barycentre.
      //               Real period ~80 yr, separation ~11–36 AU (eccentric).
      //   P         = Proxima Centauri — wide slow orbit around the AB pair.
      //               Real period ~547,000 yr, separation ~13,000 AU.
      //   G         = Proxima b — orbits PROXIMA, not the system. Real period
      //               11.186 d, semi-major axis 0.0485 AU. So fast that on
      //               this loader it should clearly whip around P.
      // We compress those three timescales into something the eye can read,
      // but preserve their *ordering*: G ≫ F↔A ≫ P.
      const t = (now - this._t0) * 0.001 * this._speed * (this._reduceMotion ? 0 : 1);

      // Angular velocities (rad/s on screen). Real ratios are wildly extreme;
      // we keep the *ordering* and use a 3-decade compression.
      const omegaG    = 1.40;   // Proxima b around Proxima — fastest
      const omegaFA   = 0.55;   // α Cen A↔B around their barycentre
      const omegaP    = 0.085;  // Proxima around AB — slowest, but visible

      const phaseFA = t * omegaFA;
      const phaseP  = t * omegaP + Math.PI * 0.85;
      const phaseG  = t * omegaG;

      // Eccentric α Cen AB: real e ≈ 0.52. Tight binary, kept small at centre.
      const eFA  = 0.52;
      const aFA  = 4.5 * unit;                          // semi-major (screen)
      const rFA  = aFA * (1 - eFA * eFA) / (1 + eFA * Math.cos(phaseFA));
      const massA = 1.10, massB = 0.91;                 // M⊙ (real)
      const totalAB = massA + massB;

      // Proxima C's orbit is THE outer ring — the same circle the progress arc
      // is drawn on. Slight eccentricity, no inclination foreshortening so it
      // reads as a clean ring, not a squashed ellipse.
      const eP   = 0.0;                                 // circular for clean visual
      const aP   = 38 * unit;                           // shared with progress arc
      const rP   = aP;                                  // circular: r = a
      const inclP = 1.0;                                // face-on

      // Proxima b: tight circular orbit around Proxima C.
      const aG   = 6.4 * unit;
      const inclG = 0.42;

      // On settle: orbits decay inward slightly, then triangle locks on.
      const settleEase = settle < 0 ? 0 : (1 - Math.pow(1 - settle, 3));

      // ---- Faint orbit rings ----
      // We DON'T draw P's ring here — the progress arc on the outer rule IS
      // Proxima C's orbit. Drawing it here too would double up.
      ctx.save();
      ctx.globalAlpha = 0.5 * (1 - settleEase * 0.5);
      ctx.strokeStyle = p.triLine;
      ctx.lineWidth = 0.25 * unit;
      ctx.setLineDash([0.4 * unit, 0.7 * unit]);

      // F↔A binary orbit — small eccentric ellipse, focus at barycentre.
      const cFA = aFA * eFA;
      ctx.save();
      ctx.translate(cx, cy);
      ctx.beginPath();
      ctx.ellipse(-cFA, 0, aFA, aFA * Math.sqrt(1 - eFA * eFA), 0, 0, Math.PI * 2);
      ctx.stroke();
      ctx.restore();
      ctx.setLineDash([]);
      ctx.restore();

      // ---- Positions ----
      // Barycentre of the WHOLE system is the centre of the canvas.
      const sysX = cx, sysY = cy;

      // F & A around their barycentre. F = A-star (heavier), A = B-star (lighter)
      // — yes, the labels are inverted vs astronomical naming, but in our seal
      //   F (Facts) is slate-bone and A (Abstractions) is amber, which we keep.
      const abAngle = phaseFA;
      // F sits opposite A across the barycentre, scaled by the mass ratio.
      const fX = sysX + Math.cos(abAngle + Math.PI) * rFA * (massB / totalAB);
      const fY = sysY + Math.sin(abAngle + Math.PI) * rFA * (massB / totalAB);
      const aX = sysX + Math.cos(abAngle)            * rFA * (massA / totalAB);
      const aY = sysY + Math.sin(abAngle)            * rFA * (massA / totalAB);

      // P on its (circular) orbit — the same ring as the progress arc.
      const pX = sysX + Math.cos(phaseP) * rP;
      const pY = sysY + Math.sin(phaseP) * rP;

      // G — Proxima b — orbits Proxima itself.
      const gX = pX + Math.cos(phaseG) * aG;
      const gY = pY + Math.sin(phaseG) * aG * inclG;

      // ---- Inscribed triangle waking up between F · A · P ----
      // Subtle in spinning state, pulses bright on settle. Triangle endpoints
      // are the live star positions — it deforms as the system orbits.
      //
      // Progress arc is drawn FIRST so the bodies that follow (P, planet)
      // sit visibly on top of it.
      this._drawProgressArc(ctx, cx, cy, unit, p, t, settleEase);

      ctx.save();
      const triAlpha = 0.18 + 0.18 * (0.5 + 0.5 * Math.sin(t * 0.6))
                     + 0.55 * settleEase;
      ctx.globalAlpha = triAlpha;
      ctx.strokeStyle = p.triLine;
      ctx.lineWidth = 0.3 * unit;
      ctx.setLineDash([0.4 * unit, 0.8 * unit]);
      ctx.beginPath();
      ctx.moveTo(pX, pY);
      ctx.lineTo(fX, fY);
      ctx.moveTo(pX, pY);
      ctx.lineTo(aX, aY);
      ctx.stroke();
      ctx.setLineDash([]);
      ctx.beginPath();
      ctx.moveTo(fX, fY);
      ctx.lineTo(aX, aY);
      ctx.stroke();

      // F<->A binary tether (gravitational binding line, dimmed)
      ctx.globalAlpha = 0.35 + 0.25 * settleEase;
      ctx.strokeStyle = p.starA;
      ctx.lineWidth = 0.3 * unit;
      ctx.beginPath();
      ctx.moveTo(fX, fY);
      ctx.lineTo(aX, aY);
      ctx.stroke();
      ctx.restore();

      // ---- Proxima b's orbit ring around P (small dotted ellipse) ----
      ctx.save();
      ctx.globalAlpha = 0.5 * (1 - settleEase * 0.4);
      ctx.strokeStyle = p.orbit;
      ctx.lineWidth = 0.25 * unit;
      ctx.setLineDash([0.4 * unit, 0.7 * unit]);
      ctx.beginPath();
      ctx.ellipse(pX, pY, aG, aG * inclG, 0, 0, Math.PI * 2);
      ctx.stroke();
      ctx.setLineDash([]);
      ctx.restore();

      // ---- Goal planet (Proxima b, orbiting P) — z-ordered against P ----
      // When the planet is on the FAR side of P (y-offset negative on the
      // tilted orbit → sin(phaseG) < 0), draw it first so P occludes it.
      // On the NEAR side, draw it after P so it passes IN FRONT.
      const planetIsBehind = Math.sin(phaseG) < 0;
      if (planetIsBehind) {
        this._drawPlanet(ctx, gX, gY, unit, p, t, settleEase, phaseG);
      }

      // ---- Stars: F (slate-bone), A (warm amber-bone), P (bone-bright) ----
      // P trail — along the (circular) outer orbit.
      this._drawTrail(ctx, p.starP, (a) => {
        const ph = phaseP - a;
        return [sysX + Math.cos(ph) * rP, sysY + Math.sin(ph) * rP];
      }, 1.6, 0.55, unit);

      // F (heavier) trail — inverted phase, scaled by mass.
      this._drawTrail(ctx, p.starF, (a) => {
        const ph = phaseFA - a;
        const r = aFA * (1 - eFA * eFA) / (1 + eFA * Math.cos(ph));
        return [
          sysX + Math.cos(ph + Math.PI) * r * (massB / totalAB),
          sysY + Math.sin(ph + Math.PI) * r * (massB / totalAB),
        ];
      }, 1.2, 0.32, unit);

      // A (lighter) trail.
      this._drawTrail(ctx, p.starA, (a) => {
        const ph = phaseFA - a;
        const r = aFA * (1 - eFA * eFA) / (1 + eFA * Math.cos(ph));
        return [
          sysX + Math.cos(ph) * r * (massA / totalAB),
          sysY + Math.sin(ph) * r * (massA / totalAB),
        ];
      }, 1.2, 0.32, unit);

      // F · slate-bone (heavier of the binary, slightly larger)
      ctx.beginPath();
      ctx.fillStyle = p.starF;
      ctx.arc(fX, fY, 2.2 * unit, 0, Math.PI * 2);
      ctx.fill();

      // A · warm amber-bone (lighter, slightly smaller)
      ctx.beginPath();
      ctx.fillStyle = p.starA;
      ctx.arc(aX, aY, 1.9 * unit, 0, Math.PI * 2);
      ctx.fill();

      // P · bone-bright + 4-ray glint
      ctx.beginPath();
      ctx.fillStyle = p.starP;
      ctx.arc(pX, pY, 2.6 * unit, 0, Math.PI * 2);
      ctx.fill();

      // If the planet is on the NEAR side of its orbit, draw it now — in
      // front of P. (Far-side render happened above, behind P.)
      if (!planetIsBehind) {
        this._drawPlanet(ctx, gX, gY, unit, p, t, settleEase, phaseG);
      }
      // Glints
      ctx.save();
      ctx.strokeStyle = p.starP;
      ctx.lineWidth = 0.4 * unit;
      ctx.globalAlpha = 0.7 + 0.3 * Math.sin(t * 2.2);
      const gl = 1.4 * unit, glIn = 4.2 * unit, glOut = 5.6 * unit;
      ctx.beginPath();
      ctx.moveTo(pX, pY - glIn); ctx.lineTo(pX, pY - glOut);
      ctx.moveTo(pX, pY + glIn); ctx.lineTo(pX, pY + glOut);
      ctx.moveTo(pX - glIn, pY); ctx.lineTo(pX - glOut, pY);
      ctx.moveTo(pX + glIn, pY); ctx.lineTo(pX + glOut, pY);
      ctx.stroke();
      ctx.restore();

      // ---- Progress arc on the outer rule ----
      // Drawn BEFORE the bodies so Proxima C and its planet always sit on top
      // of the loading bar, never behind it.
      // (Moved up from end-of-frame.)

      // ---- Settle flash — P's orbit (the progress arc) brightens briefly ----
      if (this._settling) {
        const flash = Math.sin(settle * Math.PI); // 0..1..0
        ctx.save();
        ctx.globalAlpha = 0.18 * flash;
        ctx.beginPath();
        ctx.arc(cx, cy, 38 * unit, 0, Math.PI * 2);
        ctx.lineWidth = 0.8 * unit;
        ctx.strokeStyle = p.arc;
        ctx.stroke();
        ctx.restore();
      }
    }

    _drawPlanet(ctx, cx, cy, unit, p, t, settle, phaseG = 0) {
      // Smaller planet — it now rides the outer ring with Proxima C, so it
      // must fit comfortably without crowding the rule or the star.
      const tilt = Math.sin(phaseG * 0.5) * 0.18;
      const bodyR = 1.8 * unit;
      const ringRx = 3.6 * unit;
      const ringRy = 0.85 * unit;
      // Ring (back half)
      ctx.save();
      ctx.translate(cx, cy);
      ctx.rotate(tilt);
      ctx.beginPath();
      ctx.lineWidth = 0.4 * unit;
      ctx.strokeStyle = p.planetRim;
      ctx.ellipse(0, 0, ringRx, ringRy, 0, 0, Math.PI * 2);
      ctx.stroke();
      ctx.restore();

      // Body
      ctx.beginPath();
      ctx.fillStyle = p.planet;
      ctx.arc(cx, cy, bodyR, 0, Math.PI * 2);
      ctx.fill();

      // Terminator (night side crescent)
      ctx.save();
      ctx.translate(cx, cy);
      ctx.rotate(-0.35 + Math.sin(t * 0.35) * 0.25);
      ctx.globalAlpha = 0.6;
      ctx.fillStyle = p.planetRim;
      ctx.beginPath();
      ctx.ellipse(0.2 * unit, 0, 1.4 * unit, bodyR, 0, -Math.PI / 2, Math.PI / 2);
      ctx.fill();
      ctx.restore();

      // Ring (front half)
      ctx.save();
      ctx.translate(cx, cy);
      ctx.rotate(tilt);
      ctx.beginPath();
      ctx.lineWidth = 0.4 * unit;
      ctx.strokeStyle = p.planetRim;
      ctx.moveTo(-ringRx, 0);
      ctx.quadraticCurveTo(0, ringRy * 1.9, ringRx, 0);
      ctx.stroke();
      ctx.restore();

      // Settle: faint pulse around the planet
      if (settle > 0) {
        ctx.save();
        ctx.globalAlpha = 0.35 * Math.sin(settle * Math.PI);
        ctx.beginPath();
        ctx.lineWidth = 0.4 * unit;
        ctx.strokeStyle = p.planet;
        ctx.arc(cx, cy, (2.6 + 2.5 * settle) * unit, 0, Math.PI * 2);
        ctx.stroke();
        ctx.restore();
      }
    }

    // Draw a fading trail by sampling N points back along the orbit.
    _drawTrail(ctx, color, posAtAngle, lengthRad, baseAlpha, unit) {
      const N = 14;
      ctx.save();
      ctx.fillStyle = color;
      for (let i = 1; i <= N; i++) {
        const a = (i / N) * lengthRad;
        const [x, y] = posAtAngle(a);
        const k = 1 - i / N;
        ctx.globalAlpha = baseAlpha * k * k;
        ctx.beginPath();
        ctx.arc(x, y, 0.7 * unit * k + 0.2 * unit, 0, Math.PI * 2);
        ctx.fill();
      }
      ctx.restore();
    }

    _drawProgressArc(ctx, cx, cy, unit, p, t, settle) {
      // Progress arc IS Proxima C's orbit — same radius as aP (38u).
      const r = 38 * unit;
      ctx.save();
      ctx.lineWidth = 0.8 * unit;
      ctx.strokeStyle = p.arc;
      ctx.lineCap = "butt";

      if (this._progress == null && !this._settling) {
        // Indeterminate: a comet-arc that sweeps the rule
        const head = -Math.PI / 2 + (t * 0.9) % (Math.PI * 2);
        const len = Math.PI * 0.42;
        // Fade the tail with a series of segments
        const SEG = 22;
        for (let i = 0; i < SEG; i++) {
          const a0 = head - len * (i / SEG);
          const a1 = head - len * ((i + 1) / SEG);
          ctx.globalAlpha = 0.7 * (1 - i / SEG) * (1 - i / SEG);
          ctx.beginPath();
          ctx.arc(cx, cy, r, a1, a0);
          ctx.stroke();
        }
      } else {
        // Determinate: arc from top (-π/2), clockwise.
        const prog = this._settling ? 1 : this._progSmooth;
        const start = -Math.PI / 2;
        const end = start + prog * Math.PI * 2;
        ctx.globalAlpha = 0.85;
        ctx.beginPath();
        ctx.arc(cx, cy, r, start, end);
        ctx.stroke();

        // Tick at the head
        if (prog > 0 && prog < 1) {
          ctx.globalAlpha = 1;
          const hx = cx + Math.cos(end) * r;
          const hy = cy + Math.sin(end) * r;
          ctx.beginPath();
          ctx.fillStyle = p.arc;
          ctx.arc(hx, hy, 0.9 * unit, 0, Math.PI * 2);
          ctx.fill();
        }
      }
      ctx.restore();
    }
  }

  if (!customElements.get("proxima-loader")) {
    customElements.define("proxima-loader", ProximaLoader);
  }
})();
