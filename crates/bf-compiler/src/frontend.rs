use std::error::Error;
use std::fmt;

use crate::continuation_lowering::lower_hir;
use crate::{AbiCodegenError, ContinuationProgram, compile_continuations, lexer, parser, semantic};

/// A source-level compilation error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrontendError {
    offset: Option<usize>,
    message: String,
}

impl FrontendError {
    pub(crate) fn at(offset: usize, message: impl Into<String>) -> Self {
        Self {
            offset: Some(offset),
            message: message.into(),
        }
    }

    fn without_offset(message: impl Into<String>) -> Self {
        Self {
            offset: None,
            message: message.into(),
        }
    }

    pub const fn offset(&self) -> Option<usize> {
        self.offset
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for FrontendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(offset) = self.offset {
            write!(f, "{} at byte offset {offset}", self.message)
        } else {
            f.write_str(&self.message)
        }
    }
}

impl Error for FrontendError {}

/// Error returned by [`compile_source`].
#[derive(Debug)]
pub enum SourceCompileError {
    Frontend(FrontendError),
    AbiCodegen(AbiCodegenError),
}

impl fmt::Display for SourceCompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Frontend(error) => error.fmt(f),
            Self::AbiCodegen(error) => error.fmt(f),
        }
    }
}

impl Error for SourceCompileError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Frontend(error) => Some(error),
            Self::AbiCodegen(error) => Some(error),
        }
    }
}

impl From<FrontendError> for SourceCompileError {
    fn from(error: FrontendError) -> Self {
        Self::Frontend(error)
    }
}

impl From<AbiCodegenError> for SourceCompileError {
    fn from(error: AbiCodegenError) -> Self {
        Self::AbiCodegen(error)
    }
}

/// Parse and lower BFC source to validated continuation IR.
pub fn lower_source(source: &str) -> Result<ContinuationProgram, FrontendError> {
    let tokens = lexer::lex(source)?;
    let ast = parser::parse(tokens)?;
    let hir = semantic::analyze(&ast)?;
    lower_hir(&hir).map_err(|error| FrontendError::without_offset(error.to_string()))
}

/// Compile BFC source directly to Brainfuck source.
pub fn compile_source(source: &str) -> Result<String, SourceCompileError> {
    let program = lower_source(source)?;
    Ok(compile_continuations(&program)?)
}

#[cfg(test)]
mod tests {
    use bf_interpreter::run;

    use super::*;

    fn main_source(body: &str) -> String {
        format!("void main() {{\n{body}\n}}")
    }

    fn execute(source: &str, input: &[u8]) -> Vec<u8> {
        let brainfuck = compile_source(&main_source(source)).unwrap();
        run(brainfuck.as_bytes(), input).unwrap()
    }

    #[test]
    fn compiles_an_echo_loop() {
        let source = r#"
            cell ch;
            ch = input();
            while (ch != 0) {
                output(ch);
                ch = input();
            }
        "#;
        assert_eq!(execute(source, b"hello"), b"hello");
    }

    #[test]
    fn arithmetic_wraps_and_variable_reads_are_non_destructive() {
        let source = r#"
            cell original = 'A';
            cell next = original + 1;
            output(original);
            output(next);
            original -= 66;
            output(original);
        "#;
        assert_eq!(execute(source, b""), vec![b'A', b'B', 255]);
    }

    #[test]
    fn if_else_not_and_nonzero_comparison_work() {
        let source = r#"
            cell value = input();
            if (value != 0) {
                output('T');
            } else {
                output('F');
            }
            if (!value) {
                output('0');
            } else {
                output('1');
            }
            output(value);
        "#;
        assert_eq!(execute(source, &[7]), vec![b'T', b'1', 7]);
        assert_eq!(execute(source, &[0]), vec![b'F', b'0', 0]);
    }

    #[test]
    fn all_unsigned_comparisons_handle_boundaries_and_preserve_operands() {
        let source = r#"
            cell left = input();
            cell right = input();
            output(left == right);
            output(left != right);
            output(left < right);
            output(left <= right);
            output(left > right);
            output(left >= right);
            output(left);
            output(right);
        "#;
        let brainfuck = compile_source(&main_source(source)).unwrap();
        let cases = [
            (0, 0),
            (0, 1),
            (1, 0),
            (1, 1),
            (1, 2),
            (2, 1),
            (0, 255),
            (255, 0),
            (254, 255),
            (255, 255),
        ];

        for (left, right) in cases {
            let output = run(brainfuck.as_bytes(), &[left, right]).unwrap();
            assert_eq!(
                output,
                vec![
                    u8::from(left == right),
                    u8::from(left != right),
                    u8::from(left < right),
                    u8::from(left <= right),
                    u8::from(left > right),
                    u8::from(left >= right),
                    left,
                    right,
                ],
                "comparison results for {left} and {right}",
            );
        }
    }

    #[test]
    fn logical_operators_short_circuit_and_return_booleans() {
        let source = r#"
            cell zero = 0;
            cell one = 1;
            output(zero && input());
            output(input());
            output(one || input());
            output(input());
            output(one && input());
            output(zero || input());
        "#;
        assert_eq!(
            execute(source, &[b'A', b'B', 2, 0]),
            vec![0, b'A', 1, b'B', 1, 0]
        );
    }

    #[test]
    fn stage_four_operator_precedence_matches_the_specification() {
        let source = r#"
            output(1 || 0 && 0);
            output(2 == 1 < 2);
            output(1 + 2 < 4 == 1);
        "#;
        assert_eq!(execute(source, b""), vec![1, 0, 1]);
    }

    #[test]
    fn blocks_shadow_outer_variables() {
        let source = r#"
            // 外側の値は内側のブロックを抜けても残る。
            cell value = 1;
            {
                cell value = 2;
                /* 日本語の
                   ブロックコメント */
                output(value);
            }
            output(value);
        "#;
        assert_eq!(execute(source, b""), vec![2, 1]);
    }

    #[test]
    fn reports_source_errors_with_offsets() {
        let undefined_source = main_source("output(missing);");
        let undefined = lower_source(&undefined_source).unwrap_err();
        assert_eq!(undefined.offset(), undefined_source.find("missing"));
        assert!(undefined.message().contains("undefined variable"));

        let undefined_function = lower_source(&main_source("future_function(1);")).unwrap_err();
        assert!(undefined_function.message().contains("undefined function"));

        let future_definition = lower_source("void helper() {}").unwrap_err();
        assert!(future_definition.message().contains("must define"));

        let unbraced_declaration =
            lower_source(&main_source("cell x; if (x) cell y;")).unwrap_err();
        assert!(unbraced_declaration.message().contains("inside a block"));

        let comment = lower_source(&main_source("/* no end")).unwrap_err();
        assert!(comment.message().contains("unterminated block comment"));

        let non_ascii_code = lower_source(&main_source("cell 値;")).unwrap_err();
        assert!(
            non_ascii_code
                .message()
                .contains("only allowed inside comments")
        );
    }

    #[test]
    fn requires_exactly_one_parameterless_void_main() {
        let empty = lower_source("").unwrap_err();
        assert!(empty.message().contains("void main()"));

        let top_level_statement = lower_source("output('x');").unwrap_err();
        assert!(top_level_statement.message().contains("'cell' or 'void'"));

        let parameter = lower_source("void main(cell value) {}").unwrap_err();
        assert!(parameter.message().contains("must not have parameters"));

        let duplicate_main = lower_source("void main() {} void main() {}").unwrap_err();
        assert!(duplicate_main.message().contains("already defined"));
    }

    #[test]
    fn scalar_and_void_calls_work_before_their_definitions() {
        let source = r#"
            void main() {
                emit(increment('A'));
            }

            void emit(cell value) {
                output(value);
            }

            cell increment(cell value) {
                return value + 1;
            }
        "#;
        let brainfuck = compile_source(source).unwrap();
        assert_eq!(run(brainfuck.as_bytes(), b"").unwrap(), b"B");
    }

    #[test]
    fn recursive_calls_preserve_callers_and_return_values() {
        let source = r#"
            cell count(cell value) {
                if (value == 0) {
                    return 1;
                }
                return count(value - 1) + 1;
            }

            void main() {
                cell original = input();
                output(count(original));
                output(original);
            }
        "#;
        let brainfuck = compile_source(source).unwrap();
        assert_eq!(run(brainfuck.as_bytes(), &[4]).unwrap(), vec![5, 4]);
    }

    #[test]
    fn constant_index_local_arrays_work_end_to_end() {
        let source = r#"
            cell use_array(cell seed) {
                cell[4] values;
                values[1 + 1] = seed;
                values[2] += 3;
                values[!1] = 9;
                values[0] -= 2;
                return values[2] + values[0];
            }

            cell recursive_array(cell depth) {
                cell[2] values;
                values[0] = depth;
                if (depth) {
                    return recursive_array(depth - 1) + values[0];
                }
                return values[1];
            }

            void main() {
                cell[2] zeroed;
                output(zeroed[1]);
                output(use_array(4));
                output(use_array(1));
                output(recursive_array(3));
                zeroed[1] = 'X';
                output(zeroed[1 || input()]);
                output(input());
            }
        "#;
        let brainfuck = compile_source(source).unwrap();
        assert_eq!(
            run(brainfuck.as_bytes(), b"Q").unwrap(),
            vec![0, 14, 11, 6, b'X', b'Q'],
        );
    }

    #[test]
    fn local_arrays_cross_chunks_with_both_abi_geometries() {
        let source = r#"
            void main() {
                cell[18] values;
                values[0] = 'A';
                values[7] = 'B';
                values[8] = 'C';
                values[15] = 'D';
                values[16] = 'E';
                values[17] = 'F';
                output(values[0]);
                output(values[7]);
                output(values[8]);
                output(values[15]);
                output(values[16]);
                output(values[17]);
            }
        "#;
        let program = lower_source(source).unwrap();

        for chunk_cells in [8, 16] {
            let config = crate::AbiConfig::new(chunk_cells).unwrap();
            let brainfuck = crate::lower_continuations_with_config(&program, config)
                .unwrap()
                .to_source();
            assert_eq!(
                run(brainfuck.as_bytes(), b"").unwrap(),
                b"ABCDEF",
                "chunk size {chunk_cells}",
            );
        }
    }

    #[test]
    fn maximum_length_local_array_runs_with_both_abi_geometries() {
        let program = lower_source(
            "void main() { cell[256] values; values[255] = 'Z'; output(values[255]); }",
        )
        .unwrap();

        for chunk_cells in [8, 16] {
            let config = crate::AbiConfig::new(chunk_cells).unwrap();
            let brainfuck = crate::lower_continuations_with_config(&program, config)
                .unwrap()
                .to_source();
            assert_eq!(
                run(brainfuck.as_bytes(), b"").unwrap(),
                b"Z",
                "chunk size {chunk_cells}",
            );
        }
    }

    #[test]
    fn mutually_recursive_scalar_calls_work_end_to_end() {
        let source = r#"
            cell even(cell value) {
                if (value == 0) {
                    return 1;
                }
                return odd(value - 1);
            }

            cell odd(cell value) {
                if (value == 0) {
                    return 0;
                }
                return even(value - 1);
            }

            void main() {
                output(even(4));
                output(odd(4));
                output(odd(5));
            }
        "#;
        let brainfuck = compile_source(source).unwrap();
        assert_eq!(run(brainfuck.as_bytes(), b"").unwrap(), vec![1, 0, 1]);
    }

    #[test]
    fn calls_in_logical_expressions_short_circuit_end_to_end() {
        let source = r#"
            cell read_boolean() {
                return input();
            }

            void main() {
                output(0 && read_boolean());
                output(input());
                output(1 || read_boolean());
                output(input());
                output(1 && read_boolean());
                output(0 || read_boolean());
            }
        "#;
        let brainfuck = compile_source(source).unwrap();
        assert_eq!(
            run(brainfuck.as_bytes(), &[b'A', b'B', 2, 0]).unwrap(),
            vec![0, b'A', 1, b'B', 1, 0],
        );
    }

    #[test]
    fn call_arguments_are_evaluated_left_to_right() {
        let source = r#"
            cell first(cell left, cell right) {
                return left;
            }

            void main() {
                output(first(input(), input()));
                output(input());
            }
        "#;
        let brainfuck = compile_source(source).unwrap();
        assert_eq!(run(brainfuck.as_bytes(), b"ABC").unwrap(), b"AC");
    }

    #[test]
    fn reports_invalid_call_contexts() {
        let statement_call =
            lower_source("cell helper() { return 1; } void main() { helper(); }").unwrap_err();
        assert!(
            statement_call
                .message()
                .contains("cannot be used as a statement")
        );

        let expression_call =
            lower_source("void helper() {} void main() { cell value = helper(); }").unwrap_err();
        assert!(
            expression_call
                .message()
                .contains("cannot be used as a value")
        );
    }
}
