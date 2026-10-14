//! TeX math as one line of Unicode for the terminal: `\alpha_1 \le x^2`
//! becomes `α₁ ≤ x²`. A pragmatic subset of TeX — symbols, scripts,
//! fractions, roots, accents, text and font commands, matrix environments.
//! Unknown commands stay as written, so no input is lost.

/// Render TeX math source as one line of Unicode text.
pub fn to_unicode(tex: &str) -> String {
    let mut parser = Parser {
        chars: tex.chars().collect(),
        at: 0,
    };
    let text = parser.sequence(false);
    // TeX ignores source spacing; keep single spaces, none inside brackets.
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace("( ", "(")
        .replace("[ ", "[")
        .replace(" )", ")")
        .replace(" ]", "]")
        .replace(" ;", ";")
        .replace(" ,", ",")
}

struct Parser {
    chars: Vec<char>,
    at: usize,
}

impl Parser {
    fn next(&mut self) -> Option<char> {
        let ch = self.chars.get(self.at).copied();
        self.at += usize::from(ch.is_some());
        ch
    }

    /// Atoms up to the end of input, or up to the `}` that closes a group.
    fn sequence(&mut self, group: bool) -> String {
        let mut out = String::new();
        while let Some(ch) = self.next() {
            match ch {
                '}' if group => break,
                '{' => out.push_str(&self.sequence(true)),
                '\\' => out.push_str(&self.command()),
                '^' | '_' => {
                    let argument = self.argument();
                    out.push_str(&script(&argument, ch == '^'));
                }
                // Column separators and non-breaking spaces.
                '&' | '~' => out.push(' '),
                _ => out.push(ch),
            }
        }
        out
    }

    /// The next argument: a `{…}` group, a command, or one character.
    fn argument(&mut self) -> String {
        while self.chars.get(self.at).is_some_and(|ch| ch.is_whitespace()) {
            self.at += 1;
        }
        match self.next() {
            Some('{') => self.sequence(true),
            Some('\\') => self.command(),
            Some(ch) => ch.to_string(),
            None => String::new(),
        }
    }

    /// An optional `[…]` argument, as in `\sqrt[3]{x}`.
    fn optional(&mut self) -> Option<String> {
        if self.chars.get(self.at) != Some(&'[') {
            return None;
        }
        let close = self.at + self.chars[self.at..].iter().position(|&ch| ch == ']')?;
        let inner: String = self.chars[self.at + 1..close].iter().collect();
        self.at = close + 1;
        Some(to_unicode(&inner))
    }

    /// The command after a `\`, with its arguments.
    fn command(&mut self) -> String {
        let start = self.at;
        while self
            .chars
            .get(self.at)
            .is_some_and(char::is_ascii_alphabetic)
        {
            self.at += 1;
        }
        if self.at == start && self.at < self.chars.len() {
            // A one-character command: `\,`, `\{`, `\\`.
            self.at += 1;
        }
        let name: String = self.chars[start..self.at].iter().collect();
        if let Some(symbol) = symbol(&name) {
            return symbol.to_string();
        }
        match name.as_str() {
            "frac" | "dfrac" | "tfrac" | "cfrac" => {
                let top = self.argument();
                let bottom = self.argument();
                format!("{}/{}", operand(&top), operand(&bottom))
            }
            "sqrt" => {
                let sign = match self.optional().as_deref() {
                    None => "√".to_string(),
                    Some("3") => "∛".to_string(),
                    Some("4") => "∜".to_string(),
                    Some(index) => format!("{}√", script(index, true)),
                };
                format!("{sign}{}", operand(&self.argument()))
            }
            "binom" => {
                let n = self.argument();
                let k = self.argument();
                format!("C({n}, {k})")
            }
            "mathbb" => self.argument().chars().map(double_struck).collect(),
            "begin" => {
                let env = self.argument();
                if env == "array" {
                    // The column spec, e.g. `{cc}`.
                    self.argument();
                }
                fence(&env).0.to_string()
            }
            "end" => fence(&self.argument()).1.to_string(),
            "left" | "right" | "middle" | "big" | "Big" | "bigg" | "Bigg" | "bigl" | "bigr"
            | "Bigl" | "Bigr" | "biggl" | "biggr" | "Biggl" | "Biggr" => {
                // The delimiter follows as its own atom; `.` means none.
                if self.chars.get(self.at) == Some(&'.') {
                    self.at += 1;
                }
                String::new()
            }
            "displaystyle" | "textstyle" | "scriptstyle" | "limits" | "nolimits" => String::new(),
            "text" | "textrm" | "textit" | "textbf" | "textsf" | "texttt" | "textup"
            | "textnormal" | "mathrm" | "mathit" | "mathbf" | "mathsf" | "mathtt" | "mathcal"
            | "mathfrak" | "mathscr" | "boldsymbol" | "bm" | "mbox" | "operatorname" | "emph"
            | "underline" => self.argument(),
            "sin" | "cos" | "tan" | "cot" | "sec" | "csc" | "arcsin" | "arccos" | "arctan"
            | "sinh" | "cosh" | "tanh" | "coth" | "log" | "ln" | "lg" | "exp" | "max" | "min"
            | "lim" | "limsup" | "liminf" | "sup" | "inf" | "det" | "gcd" | "lcm" | "deg"
            | "dim" | "ker" | "arg" | "hom" | "Pr" | "mod" | "bmod" => name,
            _ => match accent(&name) {
                Some(mark) => self.argument().chars().flat_map(|ch| [ch, mark]).collect(),
                None => {
                    // Unknown: keep it as written, with its brace arguments.
                    let mut out = format!("\\{name}");
                    while self.chars.get(self.at) == Some(&'{') {
                        self.at += 1;
                        out.push('{');
                        out.push_str(&self.sequence(true));
                        out.push('}');
                    }
                    out
                }
            },
        }
    }
}

/// A fraction or root operand: in parentheses unless it is one symbol or
/// a plain number.
fn operand(text: &str) -> String {
    let text = text.trim();
    let core = text.strip_prefix('√').unwrap_or(text);
    if core.chars().count() <= 1 || core.chars().all(|ch| ch.is_ascii_digit() || ch == '.') {
        text.to_string()
    } else {
        format!("({text})")
    }
}

/// A superscript or subscript: Unicode small forms when every character
/// has one, else a visible `^x` / `_(xy)`.
fn script(text: &str, sup: bool) -> String {
    let text = text.trim();
    let small: Option<String> = text
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .map(|ch| if sup { superscript(ch) } else { subscript(ch) })
        .collect();
    match small {
        Some(small) if !small.is_empty() => small,
        _ => {
            let mark = if sup { '^' } else { '_' };
            if text.chars().count() == 1 {
                format!("{mark}{text}")
            } else {
                format!("{mark}({text})")
            }
        }
    }
}

fn superscript(ch: char) -> Option<char> {
    Some(match ch {
        '0' => '⁰',
        '1' => '¹',
        '2' => '²',
        '3' => '³',
        '4' => '⁴',
        '5' => '⁵',
        '6' => '⁶',
        '7' => '⁷',
        '8' => '⁸',
        '9' => '⁹',
        '+' => '⁺',
        '-' | '−' => '⁻',
        '=' => '⁼',
        '(' => '⁽',
        ')' => '⁾',
        'a' => 'ᵃ',
        'b' => 'ᵇ',
        'c' => 'ᶜ',
        'd' => 'ᵈ',
        'e' => 'ᵉ',
        'f' => 'ᶠ',
        'g' => 'ᵍ',
        'h' => 'ʰ',
        'i' => 'ⁱ',
        'j' => 'ʲ',
        'k' => 'ᵏ',
        'l' => 'ˡ',
        'm' => 'ᵐ',
        'n' => 'ⁿ',
        'o' => 'ᵒ',
        'p' => 'ᵖ',
        'r' => 'ʳ',
        's' => 'ˢ',
        't' => 'ᵗ',
        'u' => 'ᵘ',
        'v' => 'ᵛ',
        'w' => 'ʷ',
        'x' => 'ˣ',
        'y' => 'ʸ',
        'z' => 'ᶻ',
        'A' => 'ᴬ',
        'B' => 'ᴮ',
        'D' => 'ᴰ',
        'E' => 'ᴱ',
        'G' => 'ᴳ',
        'H' => 'ᴴ',
        'I' => 'ᴵ',
        'J' => 'ᴶ',
        'K' => 'ᴷ',
        'L' => 'ᴸ',
        'M' => 'ᴹ',
        'N' => 'ᴺ',
        'O' => 'ᴼ',
        'P' => 'ᴾ',
        'R' => 'ᴿ',
        'T' => 'ᵀ',
        'U' => 'ᵁ',
        'V' => 'ⱽ',
        'W' => 'ᵂ',
        'α' => 'ᵅ',
        'β' => 'ᵝ',
        'γ' => 'ᵞ',
        'δ' => 'ᵟ',
        'θ' => 'ᶿ',
        'φ' => 'ᵠ',
        'χ' => 'ᵡ',
        '∘' => '°',
        '′' => '′',
        _ => return None,
    })
}

fn subscript(ch: char) -> Option<char> {
    Some(match ch {
        '0' => '₀',
        '1' => '₁',
        '2' => '₂',
        '3' => '₃',
        '4' => '₄',
        '5' => '₅',
        '6' => '₆',
        '7' => '₇',
        '8' => '₈',
        '9' => '₉',
        '+' => '₊',
        '-' | '−' => '₋',
        '=' => '₌',
        '(' => '₍',
        ')' => '₎',
        'a' => 'ₐ',
        'e' => 'ₑ',
        'h' => 'ₕ',
        'i' => 'ᵢ',
        'j' => 'ⱼ',
        'k' => 'ₖ',
        'l' => 'ₗ',
        'm' => 'ₘ',
        'n' => 'ₙ',
        'o' => 'ₒ',
        'p' => 'ₚ',
        'r' => 'ᵣ',
        's' => 'ₛ',
        't' => 'ₜ',
        'u' => 'ᵤ',
        'v' => 'ᵥ',
        'x' => 'ₓ',
        'β' => 'ᵦ',
        'γ' => 'ᵧ',
        'ρ' => 'ᵨ',
        'φ' => 'ᵩ',
        'χ' => 'ᵪ',
        _ => return None,
    })
}

fn double_struck(ch: char) -> char {
    match ch {
        'C' => 'ℂ',
        'H' => 'ℍ',
        'N' => 'ℕ',
        'P' => 'ℙ',
        'Q' => 'ℚ',
        'R' => 'ℝ',
        'Z' => 'ℤ',
        _ => ch,
    }
}

/// The combining mark an accent command puts over each character.
fn accent(name: &str) -> Option<char> {
    Some(match name {
        "hat" | "widehat" => '\u{302}',
        "tilde" | "widetilde" => '\u{303}',
        "bar" => '\u{304}',
        "overline" => '\u{305}',
        "dot" => '\u{307}',
        "ddot" => '\u{308}',
        "vec" => '\u{20D7}',
        _ => return None,
    })
}

/// The brackets a matrix-like environment opens and closes with.
fn fence(env: &str) -> (&'static str, &'static str) {
    match env.trim_end_matches('*') {
        "pmatrix" => ("(", ")"),
        "bmatrix" => ("[", "]"),
        "Bmatrix" => ("{", "}"),
        "vmatrix" => ("|", "|"),
        "Vmatrix" => ("‖", "‖"),
        "cases" => ("{ ", ""),
        _ => ("", ""),
    }
}

/// A command that stands for one symbol.
fn symbol(name: &str) -> Option<&'static str> {
    Some(match name {
        // Spacing and escapes.
        "," | ";" | ":" | " " | "quad" | "qquad" => " ",
        "!" => "",
        "\\" => "; ",
        "{" | "lbrace" => "{",
        "}" | "rbrace" => "}",
        "%" => "%",
        "$" => "$",
        "&" => "&",
        "#" => "#",
        "_" => "_",
        "|" | "Vert" => "‖",
        "vert" | "mid" => "|",
        "colon" => ":",
        "backslash" => "\\",
        // Greek.
        "alpha" => "α",
        "beta" => "β",
        "gamma" => "γ",
        "delta" => "δ",
        "epsilon" | "varepsilon" => "ε",
        "zeta" => "ζ",
        "eta" => "η",
        "theta" => "θ",
        "vartheta" => "ϑ",
        "iota" => "ι",
        "kappa" => "κ",
        "lambda" => "λ",
        "mu" => "μ",
        "nu" => "ν",
        "xi" => "ξ",
        "pi" => "π",
        "varpi" => "ϖ",
        "rho" => "ρ",
        "varrho" => "ϱ",
        "sigma" => "σ",
        "varsigma" => "ς",
        "tau" => "τ",
        "upsilon" => "υ",
        "phi" | "varphi" => "φ",
        "chi" => "χ",
        "psi" => "ψ",
        "omega" => "ω",
        "Gamma" => "Γ",
        "Delta" => "Δ",
        "Theta" => "Θ",
        "Lambda" => "Λ",
        "Xi" => "Ξ",
        "Pi" => "Π",
        "Sigma" => "Σ",
        "Upsilon" => "Υ",
        "Phi" => "Φ",
        "Psi" => "Ψ",
        "Omega" => "Ω",
        // Operators.
        "pm" => "±",
        "mp" => "∓",
        "times" => "×",
        "div" => "÷",
        "cdot" => "·",
        "ast" => "∗",
        "star" => "⋆",
        "circ" => "∘",
        "bullet" => "•",
        "oplus" => "⊕",
        "otimes" => "⊗",
        "cup" => "∪",
        "cap" => "∩",
        "setminus" => "∖",
        "land" | "wedge" => "∧",
        "lor" | "vee" => "∨",
        "neg" | "lnot" => "¬",
        "sum" => "∑",
        "prod" => "∏",
        "coprod" => "∐",
        "int" => "∫",
        "iint" => "∬",
        "iiint" => "∭",
        "oint" => "∮",
        "partial" => "∂",
        "nabla" => "∇",
        // Relations.
        "le" | "leq" => "≤",
        "ge" | "geq" => "≥",
        "ne" | "neq" => "≠",
        "approx" => "≈",
        "sim" => "∼",
        "simeq" => "≃",
        "cong" => "≅",
        "equiv" => "≡",
        "propto" => "∝",
        "ll" => "≪",
        "gg" => "≫",
        "prec" => "≺",
        "succ" => "≻",
        "in" => "∈",
        "notin" => "∉",
        "ni" => "∋",
        "subset" => "⊂",
        "subseteq" => "⊆",
        "supset" => "⊃",
        "supseteq" => "⊇",
        "perp" | "bot" => "⊥",
        "top" => "⊤",
        "parallel" => "∥",
        "vdash" => "⊢",
        "models" => "⊨",
        // Arrows.
        "to" | "rightarrow" => "→",
        "leftarrow" | "gets" => "←",
        "leftrightarrow" => "↔",
        "Rightarrow" | "implies" => "⇒",
        "Leftarrow" => "⇐",
        "Leftrightarrow" | "iff" => "⇔",
        "mapsto" => "↦",
        "uparrow" => "↑",
        "downarrow" => "↓",
        // Delimiters.
        "langle" => "⟨",
        "rangle" => "⟩",
        "lfloor" => "⌊",
        "rfloor" => "⌋",
        "lceil" => "⌈",
        "rceil" => "⌉",
        // Other symbols.
        "infty" => "∞",
        "forall" => "∀",
        "exists" => "∃",
        "nexists" => "∄",
        "emptyset" | "varnothing" => "∅",
        "ldots" | "dots" => "…",
        "cdots" => "⋯",
        "vdots" => "⋮",
        "ddots" => "⋱",
        "angle" => "∠",
        "triangle" => "△",
        "square" => "□",
        "checkmark" => "✓",
        "prime" => "′",
        "degree" => "°",
        "hbar" => "ℏ",
        "ell" => "ℓ",
        "Re" => "ℜ",
        "Im" => "ℑ",
        "aleph" => "ℵ",
        _ => return None,
    })
}
