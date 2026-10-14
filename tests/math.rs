use harness_cli::math::to_unicode;

#[test]
fn tex_math_renders_as_one_line_of_unicode() {
    let cases = [
        (r"\alpha_1 \le x^2", "α₁ ≤ x²"),
        (
            r"x = \frac{-b \pm \sqrt{b^2 - 4ac}}{2a}",
            "x = (-b ± √(b² - 4ac))/(2a)",
        ),
        (r"\sum_{i=1}^{n} x_i^2", "∑ᵢ₌₁ⁿ xᵢ²"),
        (r"\sim 4\,\text{MB}", "∼ 4 MB"),
        (r"\frac{\pi}{2} \approx 1.57", "π/2 ≈ 1.57"),
        (r"\mathbb{R}^n \to \mathbb{R}", "ℝⁿ → ℝ"),
        (r"\hat{\theta}", "θ\u{302}"),
        (r"\left( \frac{a}{b} \right)", "(a/b)"),
        (
            r"\begin{pmatrix} a & b \\ c & d \end{pmatrix}",
            "(a b; c d)",
        ),
        (r"\sqrt[3]{x} + \sqrt{2}", "∛x + √2"),
        (r"90^\circ, 50\%", "90°, 50%"),
        // No Unicode subscript for `b`, none for `→`: a visible fallback.
        (r"x_{b}", "x_b"),
        (r"\lim_{x \to 0} \sin x", "lim_(x → 0) sin x"),
        // Unknown commands stay as written, arguments included.
        (r"\unknown{x} + 1", r"\unknown{x} + 1"),
    ];
    for (tex, expected) in cases {
        assert_eq!(to_unicode(tex), expected, "{tex}");
    }
}
