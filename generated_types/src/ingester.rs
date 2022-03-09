use crate::{
    google::{FieldViolation, FieldViolationExt, OptionalField},
    influxdata::iox::ingester::v1 as proto,
};
use data_types::timestamp::TimestampRange;
use data_types2::{IngesterQueryRequest, SequencerId};
use datafusion::logical_plan::Operator;
use predicate::{BinaryExpr, Predicate};

impl TryFrom<proto::IngesterQueryRequest> for IngesterQueryRequest {
    type Error = FieldViolation;

    fn try_from(proto: proto::IngesterQueryRequest) -> Result<Self, Self::Error> {
        let proto::IngesterQueryRequest {
            namespace,
            sequencer_id,
            table,
            columns,
            predicate,
        } = proto;

        let predicate = predicate.map(TryInto::try_into).transpose()?;
        let sequencer_id: i16 = sequencer_id.try_into().scope("sequencer_id")?;

        Ok(Self::new(
            namespace,
            SequencerId::new(sequencer_id),
            table,
            columns,
            predicate,
        ))
    }
}

impl TryFrom<IngesterQueryRequest> for proto::IngesterQueryRequest {
    type Error = FieldViolation;

    fn try_from(query: IngesterQueryRequest) -> Result<Self, Self::Error> {
        let IngesterQueryRequest {
            namespace,
            sequencer_id,
            table,
            columns,
            predicate,
        } = query;

        Ok(Self {
            namespace,
            sequencer_id: sequencer_id.get().into(),
            table,
            columns,
            predicate: predicate.map(TryInto::try_into).transpose()?,
        })
    }
}

impl TryFrom<Predicate> for proto::Predicate {
    type Error = FieldViolation;

    fn try_from(pred: Predicate) -> Result<Self, Self::Error> {
        let Predicate {
            field_columns,
            partition_key,
            range,
            exprs,
            value_expr,
        } = pred;

        let field_columns = field_columns.into_iter().flatten().collect();
        let range = range.map(|r| proto::TimestampRange {
            start: r.start(),
            end: r.end(),
        });
        let exprs = exprs
            .iter()
            .map(TryFrom::try_from)
            .collect::<Result<Vec<_>, datafusion::to_proto::Error>>()
            .map_err(|e| FieldViolation {
                field: "exprs".to_string(),
                description: e.to_string(),
            })?;
        let value_expr = value_expr
            .into_iter()
            .map(TryFrom::try_from)
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Self {
            field_columns,
            partition_key,
            range,
            exprs,
            value_expr,
        })
    }
}

impl TryFrom<proto::Predicate> for Predicate {
    type Error = FieldViolation;

    fn try_from(proto: proto::Predicate) -> Result<Self, Self::Error> {
        let proto::Predicate {
            field_columns,
            partition_key,
            range,
            exprs,
            value_expr,
        } = proto;

        let field_columns = if field_columns.is_empty() {
            None
        } else {
            Some(field_columns.into_iter().collect())
        };

        let range = range.map(|r| TimestampRange::new(r.start, r.end));

        let exprs = exprs
            .iter()
            .map(TryFrom::try_from)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e: datafusion::from_proto::Error| proto_error("exprs", e.to_string()))?;

        let value_expr = value_expr
            .iter()
            .map(|ve| {
                let left = ve.left.unwrap_field("left")?;
                let right = ve.right.unwrap_field("right")?;

                Ok(BinaryExpr {
                    left: (&left).into(),
                    op: from_proto_binary_op("op", &ve.op)?,
                    right: (&right).try_into().map_err(|e: datafusion::from_proto::Error| proto_error("right", e.to_string()))?,
                })
            })
            .collect::<Result<Vec<BinaryExpr>, FieldViolation>>()?;

        Ok(Self {
            field_columns,
            partition_key,
            range,
            exprs,
            value_expr,
        })
    }
}

impl TryFrom<BinaryExpr> for proto::BinaryExpr {
    type Error = FieldViolation;

    fn try_from(bin_expr: BinaryExpr) -> Result<Self, Self::Error> {
        let BinaryExpr { left, op, right } = bin_expr;

        Ok(Self {
            left: Some(left.into()),
            op: op.to_string(),
            right: Some((&right).try_into().map_err(|e: datafusion::to_proto::Error| proto_error("right", e.to_string()))?),
        })
    }
}

fn from_proto_binary_op(field: &'static str, op: &str) -> Result<Operator, FieldViolation> {
    match op {
        "And" => Ok(Operator::And),
        "Or" => Ok(Operator::Or),
        "Eq" => Ok(Operator::Eq),
        "NotEq" => Ok(Operator::NotEq),
        "LtEq" => Ok(Operator::LtEq),
        "Lt" => Ok(Operator::Lt),
        "Gt" => Ok(Operator::Gt),
        "GtEq" => Ok(Operator::GtEq),
        "Plus" => Ok(Operator::Plus),
        "Minus" => Ok(Operator::Minus),
        "Multiply" => Ok(Operator::Multiply),
        "Divide" => Ok(Operator::Divide),
        "Modulo" => Ok(Operator::Modulo),
        "Like" => Ok(Operator::Like),
        "NotLike" => Ok(Operator::NotLike),
        other => Err(proto_error(
            field,
            format!("Unsupported binary operator '{:?}'", other),
        )),
    }
}

fn proto_error(field: impl Into<String>, description: impl Into<String>) -> FieldViolation {
    FieldViolation {
        field: field.into(),
        description: description.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use datafusion::{logical_plan::col, protobuf::{LogicalExprNode, logical_expr_node::ExprType, Column}};

    #[test]
    fn query_from_protobuf() {
        let rust_predicate = predicate::PredicateBuilder::new()
            .timestamp_range(1, 100)
            .add_expr(col("foo"))
            .build();

        let proto_predicate = proto::Predicate {
            exprs: vec![LogicalExprNode {
                expr_type: Some(ExprType::Column(Column {
                    name: "foo".into(),
                    relation: None,
                })),
            }],
            field_columns: vec![],
            partition_key: None,
            range: Some(proto::TimestampRange { start: 1, end: 100 }),
            value_expr: vec![],
        };

        let rust_query = IngesterQueryRequest::new(
            "mydb".into(),
            SequencerId::new(5),
            "cpu".into(),
            vec!["usage".into(), "time".into()],
            Some(rust_predicate),
        );

        let proto_query = proto::IngesterQueryRequest {
            namespace: "mydb".into(),
            sequencer_id: 5,
            table: "cpu".into(),
            columns: vec!["usage".into(), "time".into()],
            predicate: Some(proto_predicate),
        };

        let rust_query_converted = IngesterQueryRequest::try_from(proto_query).unwrap();

        assert_eq!(rust_query, rust_query_converted);
    }
}
