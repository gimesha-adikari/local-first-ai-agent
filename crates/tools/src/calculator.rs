//! A small arithmetic grammar, not a scripting language.

use agent_core::agent::{AgentError, AgentErrorCode, AgentResult};

const MAX_BYTES: usize = 256;
const MAX_DEPTH: usize = 24;
const MAX_STEPS: usize = 128;

/// Evaluate finite f64 arithmetic with +, -, *, /, %, unary signs,
/// parentheses, decimal and scientific notation. Percent is remainder.
/// No variables, functions, assignments, exponent operator or code execution.
pub(crate) fn evaluate(
    expression: &str,
    mut check: impl FnMut() -> AgentResult<()>,
) -> AgentResult<f64> {
    if expression.is_empty() || expression.len() > MAX_BYTES || !expression.is_ascii() {
        return Err(invalid());
    }
    let mut parser = Parser {
        bytes: expression.as_bytes(),
        position: 0,
        steps: 0,
        check: &mut check,
    };
    let result = parser.sum(0)?;
    parser.skip_space();
    if parser.position != parser.bytes.len() {
        return Err(invalid());
    }
    (parser.check)()?;
    finite(result)
}

struct Parser<'a, F> {
    bytes: &'a [u8],
    position: usize,
    steps: usize,
    check: &'a mut F,
}

impl<F: FnMut() -> AgentResult<()>> Parser<'_, F> {
    fn step(&mut self, depth: usize) -> AgentResult<()> {
        (self.check)()?;
        self.steps += 1;
        if depth > MAX_DEPTH || self.steps > MAX_STEPS {
            return Err(AgentError::new(
                AgentErrorCode::InvalidArguments,
                "The arithmetic expression is too complex.",
            ));
        }
        Ok(())
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.position).copied()
    }

    fn skip_space(&mut self) {
        while self.peek() == Some(b' ') {
            self.position += 1;
        }
    }

    fn sum(&mut self, depth: usize) -> AgentResult<f64> {
        let mut value = self.product(depth)?;
        loop {
            self.skip_space();
            let Some(op @ (b'+' | b'-')) = self.peek() else {
                return Ok(value);
            };
            self.step(depth)?;
            self.position += 1;
            let rhs = self.product(depth)?;
            value = finite(if op == b'+' { value + rhs } else { value - rhs })?;
        }
    }

    fn product(&mut self, depth: usize) -> AgentResult<f64> {
        let mut value = self.unary(depth)?;
        loop {
            self.skip_space();
            let Some(op @ (b'*' | b'/' | b'%')) = self.peek() else {
                return Ok(value);
            };
            self.step(depth)?;
            self.position += 1;
            let rhs = self.unary(depth)?;
            if matches!(op, b'/' | b'%') && rhs == 0.0 {
                return Err(AgentError::new(
                    AgentErrorCode::InvalidArguments,
                    "Division or remainder by zero.",
                ));
            }
            value = finite(match op {
                b'*' => value * rhs,
                b'/' => value / rhs,
                _ => value % rhs,
            })?;
        }
    }

    fn unary(&mut self, depth: usize) -> AgentResult<f64> {
        self.step(depth)?;
        self.skip_space();
        match self.peek() {
            Some(b'+') => {
                self.position += 1;
                self.unary(depth + 1)
            }
            Some(b'-') => {
                self.position += 1;
                finite(-self.unary(depth + 1)?)
            }
            Some(b'(') => {
                self.position += 1;
                let value = self.sum(depth + 1)?;
                self.skip_space();
                if self.peek() != Some(b')') {
                    return Err(invalid());
                }
                self.position += 1;
                Ok(value)
            }
            _ => self.number(),
        }
    }

    fn digits(&mut self) -> usize {
        let start = self.position;
        while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
            self.position += 1;
        }
        self.position - start
    }

    fn number(&mut self) -> AgentResult<f64> {
        let start = self.position;
        let mut digits = self.digits();
        if self.peek() == Some(b'.') {
            self.position += 1;
            digits += self.digits();
        }
        if digits == 0 {
            return Err(invalid());
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.position += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.position += 1;
            }
            if self.digits() == 0 {
                return Err(invalid());
            }
        }
        let text = std::str::from_utf8(&self.bytes[start..self.position]).map_err(|_| invalid())?;
        let value = text.parse::<f64>().map_err(|_| invalid())?;
        finite(value)
    }
}

fn invalid() -> AgentError {
    AgentError::new(
        AgentErrorCode::InvalidArguments,
        "Invalid arithmetic expression.",
    )
}

fn finite(value: f64) -> AgentResult<f64> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(AgentError::new(
            AgentErrorCode::InvalidArguments,
            "The arithmetic result is not finite.",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(input: &str) -> AgentResult<f64> {
        evaluate(input, || Ok(()))
    }

    #[test]
    fn precedence_unary_decimal_and_remainder() {
        for (expression, expected) in [
            ("2+3*4", 14.0),
            ("(2+3)*4", 20.0),
            ("-(-2)+.5*2e1", 12.0),
            ("8/4/2", 1.0),
            ("8%3", 2.0),
            ("(1500 + 2750) * 0.15", 637.5),
        ] {
            assert_eq!(run(expression).unwrap(), expected);
        }
    }

    #[test]
    fn multiplying_a_sum_requires_the_parenthesized_group() {
        assert_eq!(run("19 * (23 + 7)").unwrap(), 570.0);
        assert_eq!(run("19 * 23 + 7").unwrap(), 444.0);
    }

    #[test]
    fn rejects_invalid_nonfinite_and_complex_expressions() {
        for input in [
            "",
            "1/0",
            "1%-0",
            "1e999",
            "1e308*1e308",
            "NaN",
            "2**3",
            "sqrt(4)",
            "2;3",
            "1e+",
            "1 2",
            "(2",
            "2)",
            "2(3)",
            ".",
        ] {
            assert!(run(input).is_err(), "{input}");
        }
        assert!(run(&format!("{}1{}", "(".repeat(25), ")".repeat(25))).is_err());
        assert!(run(&"+".repeat(257)).is_err());
        assert!(run(&format!("{}1", "1+".repeat(100))).is_err());
    }

    #[test]
    fn cancellation_is_propagated() {
        let mut calls = 0;
        let error = evaluate("1+2+3", || {
            calls += 1;
            if calls >= 3 {
                Err(AgentError::new(
                    AgentErrorCode::Cancelled,
                    "The task was cancelled.",
                ))
            } else {
                Ok(())
            }
        })
        .unwrap_err();
        assert_eq!(error.code, AgentErrorCode::Cancelled);
    }
}
