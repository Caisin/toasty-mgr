//! Type-safe dynamic query specifications and offset pagination.

use std::{error::Error, fmt};

/// One-based offset pagination request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Paging {
    /// One-based page number.
    pub page: u64,
    /// Maximum number of records returned in the page.
    pub size: u64,
}

/// Offset-paginated query result.
#[derive(Debug)]
pub struct Page<T> {
    /// Records in the requested page.
    pub items: Vec<T>,
    /// Validated pagination request.
    pub paging: Paging,
    /// Number of records matching the filter without pagination.
    pub total: u64,
    /// Number of pages at the requested page size.
    pub total_pages: u64,
}

/// Validation failure while constructing a generated query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TcQueryBuildError {
    /// Page numbers start at one.
    InvalidPageNumber,
    /// Page size is zero or exceeds the query specification's maximum.
    InvalidPageSize {
        /// Rejected page size.
        size: u64,
        /// Largest accepted page size.
        max: u64,
    },
    /// Page and size cannot be represented by Toasty's `usize` offset API.
    OffsetOverflow,
    /// A sort field was added more than once.
    DuplicateSort {
        /// Duplicate model field name.
        field: &'static str,
    },
}

impl fmt::Display for TcQueryBuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPageNumber => f.write_str("page number must be at least 1"),
            Self::InvalidPageSize { size, max } => {
                write!(f, "page size {size} is outside 1..={max}")
            }
            Self::OffsetOverflow => f.write_str("page offset does not fit in usize"),
            Self::DuplicateSort { field } => write!(f, "duplicate sort field `{field}`"),
        }
    }
}

impl Error for TcQueryBuildError {}

/// Failure while validating or executing a generated query.
#[derive(Debug)]
pub enum TcQueryError {
    /// Query specification validation failed before execution.
    Build(TcQueryBuildError),
    /// Toasty failed to execute the generated statement.
    Toasty(crate::Error),
}

impl fmt::Display for TcQueryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Build(error) => error.fmt(f),
            Self::Toasty(error) => error.fmt(f),
        }
    }
}

impl Error for TcQueryError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Build(error) => Some(error),
            Self::Toasty(error) => Some(error),
        }
    }
}

impl From<TcQueryBuildError> for TcQueryError {
    fn from(error: TcQueryBuildError) -> Self {
        Self::Build(error)
    }
}

impl From<crate::Error> for TcQueryError {
    fn from(error: crate::Error) -> Self {
        Self::Toasty(error)
    }
}

#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TcQuerySortDirection {
    /// Ascending order.
    Asc,
    /// Descending order.
    Desc,
}
