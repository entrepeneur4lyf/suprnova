//! Fluent assertion API inspired by Jest's expect
//!
//! Provides a fluent API for assertions with clear expected/received output.

use std::fmt::Debug;

std::thread_local! {
    /// Thread-local storage for current test name (set by test! macro)
    pub static CURRENT_TEST_NAME: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// Set the current test name (called by test! macro)
pub fn set_current_test_name(name: Option<String>) {
    CURRENT_TEST_NAME.with(|cell| {
        *cell.borrow_mut() = name;
    });
}

/// Get the current test name for error messages
fn get_test_name() -> Option<String> {
    CURRENT_TEST_NAME.with(|cell| cell.borrow().clone())
}

/// Format the assertion failure header
fn format_header(location: &str) -> String {
    if let Some(name) = get_test_name() {
        format!("\nTest: \"{}\"\n  at {}\n", name, location)
    } else {
        format!("\nassertion failed at {}\n", location)
    }
}

/// The main Expect wrapper for fluent assertions
pub struct Expect<T> {
    value: T,
    location: &'static str,
}

impl<T> Expect<T> {
    /// Create a new Expect wrapper (use the expect! macro instead)
    pub fn new(value: T, location: &'static str) -> Self {
        Self { value, location }
    }
}

// Equality matchers for Debug + PartialEq types
impl<T: Debug + PartialEq> Expect<T> {
    /// Assert that the value equals the expected value
    ///
    /// # Example
    /// ```rust,ignore
    /// expect!(actual).to_equal(expected);
    /// ```
    pub fn to_equal(&self, expected: T) {
        if self.value != expected {
            panic!(
                "{}\n  expect!(actual).to_equal(expected)\n\n  Expected: {:?}\n  Received: {:?}\n",
                format_header(self.location),
                expected,
                self.value
            );
        }
    }

    /// Assert that the value does not equal the unexpected value
    ///
    /// # Example
    /// ```rust,ignore
    /// expect!(actual).to_not_equal(unexpected);
    /// ```
    pub fn to_not_equal(&self, unexpected: T) {
        if self.value == unexpected {
            panic!(
                "{}\n  expect!(actual).to_not_equal(value)\n\n  Expected NOT: {:?}\n  Received: {:?}\n",
                format_header(self.location),
                unexpected,
                self.value
            );
        }
    }
}

// Boolean matchers
impl Expect<bool> {
    /// Assert that the value is true
    ///
    /// # Example
    /// ```rust,ignore
    /// expect!(condition).to_be_true();
    /// ```
    pub fn to_be_true(&self) {
        if !self.value {
            panic!(
                "{}\n  expect!(value).to_be_true()\n\n  Expected: true\n  Received: false\n",
                format_header(self.location)
            );
        }
    }

    /// Assert that the value is false
    ///
    /// # Example
    /// ```rust,ignore
    /// expect!(condition).to_be_false();
    /// ```
    pub fn to_be_false(&self) {
        if self.value {
            panic!(
                "{}\n  expect!(value).to_be_false()\n\n  Expected: false\n  Received: true\n",
                format_header(self.location)
            );
        }
    }
}

// Option matchers
impl<T: Debug> Expect<Option<T>> {
    /// Assert that the Option is Some
    ///
    /// # Example
    /// ```rust,ignore
    /// expect!(option).to_be_some();
    /// ```
    pub fn to_be_some(&self) {
        if self.value.is_none() {
            panic!(
                "{}\n  expect!(option).to_be_some()\n\n  Expected: Some(_)\n  Received: None\n",
                format_header(self.location)
            );
        }
    }

    /// Assert that the Option is None
    ///
    /// # Example
    /// ```rust,ignore
    /// expect!(option).to_be_none();
    /// ```
    pub fn to_be_none(&self) {
        if let Some(ref v) = self.value {
            panic!(
                "{}\n  expect!(option).to_be_none()\n\n  Expected: None\n  Received: Some({:?})\n",
                format_header(self.location),
                v
            );
        }
    }
}

// Option with PartialEq for to_contain
impl<T: Debug + PartialEq> Expect<Option<T>> {
    /// Assert that the Option contains the expected value
    ///
    /// # Example
    /// ```rust,ignore
    /// expect!(Some(5)).to_contain_value(5);
    /// ```
    pub fn to_contain_value(&self, expected: T) {
        match &self.value {
            Some(v) if *v == expected => {}
            Some(v) => {
                panic!(
                    "{}\n  expect!(option).to_contain_value(expected)\n\n  Expected: Some({:?})\n  Received: Some({:?})\n",
                    format_header(self.location),
                    expected,
                    v
                );
            }
            None => {
                panic!(
                    "{}\n  expect!(option).to_contain_value(expected)\n\n  Expected: Some({:?})\n  Received: None\n",
                    format_header(self.location),
                    expected
                );
            }
        }
    }
}

// Result matchers
impl<T: Debug, E: Debug> Expect<Result<T, E>> {
    /// Assert that the Result is Ok
    ///
    /// # Example
    /// ```rust,ignore
    /// expect!(result).to_be_ok();
    /// ```
    pub fn to_be_ok(&self) {
        if let Err(ref e) = self.value {
            panic!(
                "{}\n  expect!(result).to_be_ok()\n\n  Expected: Ok(_)\n  Received: Err({:?})\n",
                format_header(self.location),
                e
            );
        }
    }

    /// Assert that the Result is Err
    ///
    /// # Example
    /// ```rust,ignore
    /// expect!(result).to_be_err();
    /// ```
    pub fn to_be_err(&self) {
        if let Ok(ref v) = self.value {
            panic!(
                "{}\n  expect!(result).to_be_err()\n\n  Expected: Err(_)\n  Received: Ok({:?})\n",
                format_header(self.location),
                v
            );
        }
    }
}

// String matchers
impl Expect<String> {
    /// Assert that the string contains the substring
    ///
    /// # Example
    /// ```rust,ignore
    /// expect!(string).to_contain("hello");
    /// ```
    pub fn to_contain(&self, substring: &str) {
        if !self.value.contains(substring) {
            panic!(
                "{}\n  expect!(string).to_contain(substring)\n\n  Expected to contain: {:?}\n  Received: {:?}\n",
                format_header(self.location),
                substring,
                self.value
            );
        }
    }

    /// Assert that the string starts with the prefix
    ///
    /// # Example
    /// ```rust,ignore
    /// expect!(string).to_start_with("hello");
    /// ```
    pub fn to_start_with(&self, prefix: &str) {
        if !self.value.starts_with(prefix) {
            panic!(
                "{}\n  expect!(string).to_start_with(prefix)\n\n  Expected to start with: {:?}\n  Received: {:?}\n",
                format_header(self.location),
                prefix,
                self.value
            );
        }
    }

    /// Assert that the string ends with the suffix
    ///
    /// # Example
    /// ```rust,ignore
    /// expect!(string).to_end_with("world");
    /// ```
    pub fn to_end_with(&self, suffix: &str) {
        if !self.value.ends_with(suffix) {
            panic!(
                "{}\n  expect!(string).to_end_with(suffix)\n\n  Expected to end with: {:?}\n  Received: {:?}\n",
                format_header(self.location),
                suffix,
                self.value
            );
        }
    }

    /// Assert that the string has the expected length
    ///
    /// # Example
    /// ```rust,ignore
    /// expect!(string).to_have_length(5);
    /// ```
    pub fn to_have_length(&self, expected: usize) {
        let actual = self.value.len();
        if actual != expected {
            panic!(
                "{}\n  expect!(string).to_have_length({})\n\n  Expected length: {}\n  Actual length: {}\n  Value: {:?}\n",
                format_header(self.location),
                expected,
                expected,
                actual,
                self.value
            );
        }
    }

    /// Assert that the string is empty
    ///
    /// # Example
    /// ```rust,ignore
    /// expect!(string).to_be_empty();
    /// ```
    pub fn to_be_empty(&self) {
        if !self.value.is_empty() {
            panic!(
                "{}\n  expect!(string).to_be_empty()\n\n  Expected: \"\"\n  Received: {:?}\n",
                format_header(self.location),
                self.value
            );
        }
    }
}

// &str matchers
impl Expect<&str> {
    /// Assert that the string contains the substring
    pub fn to_contain(&self, substring: &str) {
        if !self.value.contains(substring) {
            panic!(
                "{}\n  expect!(string).to_contain(substring)\n\n  Expected to contain: {:?}\n  Received: {:?}\n",
                format_header(self.location),
                substring,
                self.value
            );
        }
    }

    /// Assert that the string starts with the prefix
    pub fn to_start_with(&self, prefix: &str) {
        if !self.value.starts_with(prefix) {
            panic!(
                "{}\n  expect!(string).to_start_with(prefix)\n\n  Expected to start with: {:?}\n  Received: {:?}\n",
                format_header(self.location),
                prefix,
                self.value
            );
        }
    }

    /// Assert that the string ends with the suffix
    pub fn to_end_with(&self, suffix: &str) {
        if !self.value.ends_with(suffix) {
            panic!(
                "{}\n  expect!(string).to_end_with(suffix)\n\n  Expected to end with: {:?}\n  Received: {:?}\n",
                format_header(self.location),
                suffix,
                self.value
            );
        }
    }

    /// Assert that the string has the expected length
    pub fn to_have_length(&self, expected: usize) {
        let actual = self.value.len();
        if actual != expected {
            panic!(
                "{}\n  expect!(string).to_have_length({})\n\n  Expected length: {}\n  Actual length: {}\n  Value: {:?}\n",
                format_header(self.location),
                expected,
                expected,
                actual,
                self.value
            );
        }
    }

    /// Assert that the string is empty
    pub fn to_be_empty(&self) {
        if !self.value.is_empty() {
            panic!(
                "{}\n  expect!(string).to_be_empty()\n\n  Expected: \"\"\n  Received: {:?}\n",
                format_header(self.location),
                self.value
            );
        }
    }
}

// Vec matchers
impl<T: Debug + PartialEq> Expect<Vec<T>> {
    /// Assert that the Vec has the expected length
    ///
    /// # Example
    /// ```rust,ignore
    /// expect!(vec).to_have_length(3);
    /// ```
    pub fn to_have_length(&self, expected: usize) {
        let actual = self.value.len();
        if actual != expected {
            panic!(
                "{}\n  expect!(vec).to_have_length({})\n\n  Expected length: {}\n  Actual length: {}\n",
                format_header(self.location),
                expected,
                expected,
                actual
            );
        }
    }

    /// Assert that the Vec contains the item
    ///
    /// # Example
    /// ```rust,ignore
    /// expect!(vec).to_contain(&item);
    /// ```
    pub fn to_contain(&self, item: &T) {
        if !self.value.contains(item) {
            panic!(
                "{}\n  expect!(vec).to_contain(item)\n\n  Expected to contain: {:?}\n  Received: {:?}\n",
                format_header(self.location),
                item,
                self.value
            );
        }
    }

    /// Assert that the Vec is empty
    ///
    /// # Example
    /// ```rust,ignore
    /// expect!(vec).to_be_empty();
    /// ```
    pub fn to_be_empty(&self) {
        if !self.value.is_empty() {
            panic!(
                "{}\n  expect!(vec).to_be_empty()\n\n  Expected: []\n  Received: {:?}\n",
                format_header(self.location),
                self.value
            );
        }
    }
}

// Numeric comparison matchers using PartialOrd
impl<T: Debug + PartialOrd> Expect<T> {
    /// Assert that the value is greater than the expected value
    ///
    /// # Example
    /// ```rust,ignore
    /// expect!(10).to_be_greater_than(5);
    /// ```
    pub fn to_be_greater_than(&self, expected: T) {
        if self.value <= expected {
            panic!(
                "{}\n  expect!(value).to_be_greater_than(expected)\n\n  Expected: > {:?}\n  Received: {:?}\n",
                format_header(self.location),
                expected,
                self.value
            );
        }
    }

    /// Assert that the value is less than the expected value
    ///
    /// # Example
    /// ```rust,ignore
    /// expect!(5).to_be_less_than(10);
    /// ```
    pub fn to_be_less_than(&self, expected: T) {
        if self.value >= expected {
            panic!(
                "{}\n  expect!(value).to_be_less_than(expected)\n\n  Expected: < {:?}\n  Received: {:?}\n",
                format_header(self.location),
                expected,
                self.value
            );
        }
    }

    /// Assert that the value is greater than or equal to the expected value
    ///
    /// # Example
    /// ```rust,ignore
    /// expect!(10).to_be_greater_than_or_equal(10);
    /// ```
    pub fn to_be_greater_than_or_equal(&self, expected: T) {
        if self.value < expected {
            panic!(
                "{}\n  expect!(value).to_be_greater_than_or_equal(expected)\n\n  Expected: >= {:?}\n  Received: {:?}\n",
                format_header(self.location),
                expected,
                self.value
            );
        }
    }

    /// Assert that the value is less than or equal to the expected value
    ///
    /// # Example
    /// ```rust,ignore
    /// expect!(5).to_be_less_than_or_equal(5);
    /// ```
    pub fn to_be_less_than_or_equal(&self, expected: T) {
        if self.value > expected {
            panic!(
                "{}\n  expect!(value).to_be_less_than_or_equal(expected)\n\n  Expected: <= {:?}\n  Received: {:?}\n",
                format_header(self.location),
                expected,
                self.value
            );
        }
    }
}
