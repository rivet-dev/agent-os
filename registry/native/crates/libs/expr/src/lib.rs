//! expr -- evaluate expressions (POSIX)
//!
//! Recursive-descent parser for POSIX expr grammar.
//! Operators: | & = != < > <= >= + - * / % : (regex match)
//! Uses the `regex` crate for the `:` operator (anchored match).

use std::ffi::OsString;

pub fn main(args: Vec<OsString>) -> i32 {
    let str_args: Vec<String> = args
        .iter()
        .skip(1)
        .map(|a| a.to_string_lossy().to_string())
        .collect();

    if str_args.is_empty() {
        eprintln!("expr: missing operand");
        return 2;
    }

    let mut parser = Parser {
        tokens: str_args,
        pos: 0,
    };

    match parser.parse_or() {
        Ok(val) => {
            if parser.pos < parser.tokens.len() {
                eprintln!(
                    "expr: syntax error: unexpected argument '{}'",
                    parser.tokens[parser.pos]
                );
                return 2;
            }
            println!("{}", val);
            if val.is_null() {
                1
            } else {
                0
            }
        }
        Err(msg) => {
            eprintln!("expr: {}", msg);
            2
        }
    }
}

/// Represents an expr value -- either an integer or a string.
#[derive(Clone, Debug)]
enum Value {
    Int(i64),
    Str(String),
}

impl Value {
    fn is_null(&self) -> bool {
        match self {
            Value::Int(n) => *n == 0,
            Value::Str(s) => s.is_empty(),
        }
    }

    fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(n) => Some(*n),
            Value::Str(s) => s.parse::<i64>().ok(),
        }
    }

    fn as_str(&self) -> String {
        match self {
            Value::Int(n) => n.to_string(),
            Value::Str(s) => s.clone(),
        }
    }
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::Int(n) => write!(f, "{}", n),
            Value::Str(s) => write!(f, "{}", s),
        }
    }
}

struct Parser {
    tokens: Vec<String>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&str> {
        self.tokens.get(self.pos).map(|s| &s[..])
    }

    fn next(&mut self) -> Option<&str> {
        if self.pos < self.tokens.len() {
            let tok = &self.tokens[self.pos];
            self.pos += 1;
            Some(&tok[..])
        } else {
            None
        }
    }

    fn expect(&mut self, expected: &str) -> Result<(), String> {
        match self.next() {
            Some(tok) if tok == expected => Ok(()),
            Some(tok) => Err(format!(
                "syntax error: expected '{}', got '{}'",
                expected, tok
            )),
            None => Err(format!("syntax error: expected '{}'", expected)),
        }
    }

    /// expr : or_expr
    /// or_expr : and_expr ( '|' and_expr )*
    fn parse_or(&mut self) -> Result<Value, String> {
        let mut left = self.parse_and()?;
        while self.peek() == Some("|") {
            self.next();
            let right = self.parse_and()?;
            left = if !left.is_null() { left } else { right };
        }
        Ok(left)
    }

    /// and_expr : compare_expr ( '&' compare_expr )*
    fn parse_and(&mut self) -> Result<Value, String> {
        let mut left = self.parse_compare()?;
        while self.peek() == Some("&") {
            self.next();
            let right = self.parse_compare()?;
            left = if !left.is_null() && !right.is_null() {
                left
            } else {
                Value::Int(0)
            };
        }
        Ok(left)
    }

    /// compare_expr : add_expr ( ( '=' | '!=' | '<' | '>' | '<=' | '>=' ) add_expr )?
    fn parse_compare(&mut self) -> Result<Value, String> {
        let left = self.parse_add()?;
        match self.peek() {
            Some("=" | "!=" | "<" | ">" | "<=" | ">=") => {
                let op = self.next().unwrap().to_string();
                let right = self.parse_add()?;
                let result = compare_values(&left, &right, &op);
                Ok(Value::Int(if result { 1 } else { 0 }))
            }
            _ => Ok(left),
        }
    }

    /// add_expr : mul_expr ( ( '+' | '-' ) mul_expr )*
    fn parse_add(&mut self) -> Result<Value, String> {
        let mut left = self.parse_mul()?;
        loop {
            match self.peek() {
                Some("+") | Some("-") => {
                    let op = self.next().unwrap().to_string();
                    let right = self.parse_mul()?;
                    let a = left.as_int().ok_or("non-integer argument")?;
                    let b = right.as_int().ok_or("non-integer argument")?;
                    left = Value::Int(if op == "+" { a + b } else { a - b });
                }
                _ => break,
            }
        }
        Ok(left)
    }

    /// mul_expr : match_expr ( ( '*' | '/' | '%' ) match_expr )*
    fn parse_mul(&mut self) -> Result<Value, String> {
        let mut left = self.parse_match()?;
        loop {
            match self.peek() {
                Some("*") | Some("/") | Some("%") => {
                    let op = self.next().unwrap().to_string();
                    let right = self.parse_match()?;
                    let a = left.as_int().ok_or("non-integer argument")?;
                    let b = right.as_int().ok_or("non-integer argument")?;
                    if (op == "/" || op == "%") && b == 0 {
                        return Err("division by zero".to_string());
                    }
                    left = Value::Int(match &op[..] {
                        "*" => a * b,
                        "/" => a / b,
                        "%" => a % b,
                        _ => unreachable!(),
                    });
                }
                _ => break,
            }
        }
        Ok(left)
    }

    /// match_expr : primary ( ':' primary )?
    fn parse_match(&mut self) -> Result<Value, String> {
        let left = self.parse_primary()?;
        if self.peek() == Some(":") {
            self.next();
            let right = self.parse_primary()?;
            let string = left.as_str();
            let pattern = right.as_str();
            return regex_match(&string, &pattern);
        }
        Ok(left)
    }

    /// primary : '(' expr ')' | TOKEN
    fn parse_primary(&mut self) -> Result<Value, String> {
        if self.peek() == Some("(") {
            self.next();
            let val = self.parse_or()?;
            self.expect(")")?;
            return Ok(val);
        }

        match self.next() {
            Some(tok) => {
                // Try to parse as integer
                if let Ok(n) = tok.parse::<i64>() {
                    Ok(Value::Int(n))
                } else {
                    Ok(Value::Str(tok.to_string()))
                }
            }
            None => Err("syntax error: missing argument".to_string()),
        }
    }
}

fn compare_values(left: &Value, right: &Value, op: &str) -> bool {
    // If both are integers, compare numerically
    if let (Some(a), Some(b)) = (left.as_int(), right.as_int()) {
        return match op {
            "=" => a == b,
            "!=" => a != b,
            "<" => a < b,
            ">" => a > b,
            "<=" => a <= b,
            ">=" => a >= b,
            _ => false,
        };
    }
    // Otherwise compare as strings
    let a = left.as_str();
    let b = right.as_str();
    match op {
        "=" => a == b,
        "!=" => a != b,
        "<" => a < b,
        ">" => a > b,
        "<=" => a <= b,
        ">=" => a >= b,
        _ => false,
    }
}

/// POSIX expr regex match: anchored at start of string.
/// Returns the captured group \1 if present, else the match length.
fn regex_match(string: &str, pattern: &str) -> Result<Value, String> {
    // POSIX expr : always anchors pattern at start
    let anchored = format!("^(?:{})", pattern);
    let re = regex::Regex::new(&anchored).map_err(|e| format!("invalid regex: {}", e))?;

    match re.captures(string) {
        Some(caps) => {
            // If there's a capture group, return it
            if caps.len() > 1 {
                let m = caps.get(1).map_or("", |m| m.as_str());
                Ok(Value::Str(m.to_string()))
            } else {
                // Return length of match
                let m = caps.get(0).unwrap();
                Ok(Value::Int(m.as_str().len() as i64))
            }
        }
        None => {
            // No match: return "" if pattern has group, else 0
            if pattern.contains('(') {
                Ok(Value::Str(String::new()))
            } else {
                Ok(Value::Int(0))
            }
        }
    }
}
