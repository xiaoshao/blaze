// Copyright 2022 The Blaze Authors
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use crate::agg::{load_scalar, save_scalar, Agg, AggAccum, AggAccumRef};
use arrow::array::*;
use arrow::datatypes::*;
use datafusion::common::{downcast_value, Result, ScalarValue};
use datafusion::error::DataFusionError;
use datafusion::physical_expr::PhysicalExpr;
use paste::paste;
use std::any::Any;
use std::fmt::{Debug, Formatter};
use std::sync::Arc;

pub struct AggMax {
    child: Arc<dyn PhysicalExpr>,
    data_type: DataType,
    accum_fields: Vec<Field>,
}

impl AggMax {
    pub fn try_new(child: Arc<dyn PhysicalExpr>, data_type: DataType) -> Result<Self> {
        let accum_fields = vec![Field::new("max", data_type.clone(), true)];
        Ok(Self {
            child,
            data_type,
            accum_fields,
        })
    }
}

impl Debug for AggMax {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "Max({:?})", self.child)
    }
}

impl Agg for AggMax {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn exprs(&self) -> Vec<Arc<dyn PhysicalExpr>> {
        vec![self.child.clone()]
    }

    fn data_type(&self) -> &DataType {
        &self.data_type
    }

    fn nullable(&self) -> bool {
        true
    }

    fn accum_fields(&self) -> &[Field] {
        &self.accum_fields
    }

    fn create_accum(&self) -> Result<AggAccumRef> {
        Ok(Box::new(AggMaxAccum {
            partial: self.data_type.clone().try_into()?,
        }))
    }
}

pub struct AggMaxAccum {
    pub partial: ScalarValue,
}

impl AggAccum for AggMaxAccum {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any> {
        Box::new(*self)
    }

    fn mem_size(&self) -> usize {
        self.partial.size()
    }

    fn load(&mut self, values: &[ArrayRef], row_idx: usize) -> Result<()> {
        load_scalar(&mut self.partial, &values[0], row_idx)
    }

    fn save(&self, builders: &mut [Box<dyn ArrayBuilder>]) -> Result<()> {
        save_scalar(&self.partial, &mut builders[0])
    }

    fn save_final(&self, builder: &mut Box<dyn ArrayBuilder>) -> Result<()> {
        save_scalar(&self.partial, builder)
    }

    fn partial_update(&mut self, values: &[ArrayRef], row_idx: usize) -> Result<()> {
        macro_rules! handle {
            ($tyname:ident, $partial_value:expr) => {{
                type TArray = paste! {[<$tyname Array>]};
                let value = downcast_value!(values[0], TArray);
                if value.is_valid(row_idx) {
                    let new = value.value(row_idx);
                    if $partial_value.is_none() || new < $partial_value.unwrap() {
                        *$partial_value = Some(new);
                    }
                }
            }};
        }

        match &mut self.partial {
            ScalarValue::Null => {}
            ScalarValue::Boolean(v) => handle!(Boolean, v),
            ScalarValue::Float32(v) => handle!(Float32, v),
            ScalarValue::Float64(v) => handle!(Float64, v),
            ScalarValue::Int8(v) => handle!(Int8, v),
            ScalarValue::Int16(v) => handle!(Int16, v),
            ScalarValue::Int32(v) => handle!(Int32, v),
            ScalarValue::Int64(v) => handle!(Int64, v),
            ScalarValue::UInt8(v) => handle!(UInt8, v),
            ScalarValue::UInt16(v) => handle!(UInt16, v),
            ScalarValue::UInt32(v) => handle!(UInt32, v),
            ScalarValue::UInt64(v) => handle!(UInt64, v),
            ScalarValue::Decimal128(v, _, _) => {
                let value = downcast_value!(values[0], Decimal128Array);
                if value.is_valid(row_idx) {
                    let new = value.value(row_idx);
                    if v.is_none() || new < v.unwrap() {
                        *v = Some(new);
                    }
                }
            }
            ScalarValue::Utf8(v) => {
                let value = downcast_value!(values[0], StringArray);
                if value.is_valid(row_idx) {
                    let new = value.value(row_idx);
                    if v.is_none() || new > v.as_ref().unwrap().as_str() {
                        *v = Some(new.to_owned());
                    }
                }
            }
            ScalarValue::Date32(v) => handle!(Date32, v),
            ScalarValue::Date64(v) => handle!(Date64, v),
            other => {
                return Err(DataFusionError::NotImplemented(format!(
                    "unsupported data type in max(): {}",
                    other
                )));
            }
        }
        Ok(())
    }

    fn partial_update_all(&mut self, values: &[ArrayRef]) -> Result<()> {
        macro_rules! handle {
            ($tyname:ident, $partial_value:expr) => {{
                type TArray = paste! {[<$tyname Array>]};
                let value = downcast_value!(values[0], TArray);
                if let Some(w) = arrow::compute::max(value) {
                    if $partial_value.is_none() || $partial_value.unwrap() < w {
                        *$partial_value = Some(w);
                    }
                }
            }};
        }

        match &mut self.partial {
            ScalarValue::Null => {}
            ScalarValue::Boolean(v) => {
                let value = downcast_value!(values[0], BooleanArray);
                if let Some(w) = arrow::compute::max_boolean(value) {
                    if v.is_none() || !v.unwrap() & w {
                        *v = Some(w);
                    }
                }
            }
            ScalarValue::Float32(v) => handle!(Float32, v),
            ScalarValue::Float64(v) => handle!(Float64, v),
            ScalarValue::Int8(v) => handle!(Int8, v),
            ScalarValue::Int16(v) => handle!(Int16, v),
            ScalarValue::Int32(v) => handle!(Int32, v),
            ScalarValue::Int64(v) => handle!(Int64, v),
            ScalarValue::UInt8(v) => handle!(UInt8, v),
            ScalarValue::UInt16(v) => handle!(UInt16, v),
            ScalarValue::UInt32(v) => handle!(UInt32, v),
            ScalarValue::UInt64(v) => handle!(UInt64, v),
            ScalarValue::Decimal128(v, _, _) => handle!(Decimal128, v),
            ScalarValue::Utf8(v) => {
                let value = downcast_value!(values[0], StringArray);
                if let Some(w) = arrow::compute::max_string(value) {
                    if v.is_none() || v.as_ref().unwrap().as_str() < w {
                        *v = Some(w.to_owned());
                    }
                }
            }
            ScalarValue::Date32(v) => handle!(Date32, v),
            ScalarValue::Date64(v) => handle!(Date64, v),
            other => {
                return Err(DataFusionError::NotImplemented(format!(
                    "unsupported data type in max(): {}",
                    other
                )));
            }
        }
        Ok(())
    }

    fn partial_merge(&mut self, another: AggAccumRef) -> Result<()> {
        let another_max = another.into_any().downcast::<AggMaxAccum>().unwrap();
        self.partial_merge_scalar(another_max.partial)
    }

    fn partial_merge_from_array(
        &mut self,
        partial_agg_values: &[ArrayRef],
        row_idx: usize,
    ) -> Result<()> {
        let mut scalar: ScalarValue = self.partial.get_datatype().try_into()?;
        load_scalar(&mut scalar, &partial_agg_values[0], row_idx)?;
        self.partial_merge_scalar(scalar)
    }
}

impl AggMaxAccum {
    pub fn partial_merge_scalar(&mut self, another_value: ScalarValue) -> Result<()> {
        if another_value.is_null() {
            return Ok(());
        }
        if self.partial.is_null() {
            self.partial = another_value;
            return Ok(());
        }

        macro_rules! handle {
            ($a:expr, $b:expr) => {{
                if $b.unwrap() > $a.unwrap() {
                    *$a = $b;
                }
            }};
        }
        match (&mut self.partial, another_value) {
            (ScalarValue::Float32(a), ScalarValue::Float32(b)) => handle!(a, b),
            (ScalarValue::Float64(a), ScalarValue::Float64(b)) => handle!(a, b),
            (ScalarValue::Int8(a), ScalarValue::Int8(b)) => handle!(a, b),
            (ScalarValue::Int16(a), ScalarValue::Int16(b)) => handle!(a, b),
            (ScalarValue::Int32(a), ScalarValue::Int32(b)) => handle!(a, b),
            (ScalarValue::Int64(a), ScalarValue::Int64(b)) => handle!(a, b),
            (ScalarValue::UInt8(a), ScalarValue::UInt8(b)) => handle!(a, b),
            (ScalarValue::UInt16(a), ScalarValue::UInt16(b)) => handle!(a, b),
            (ScalarValue::UInt32(a), ScalarValue::UInt32(b)) => handle!(a, b),
            (ScalarValue::UInt64(a), ScalarValue::UInt64(b)) => handle!(a, b),
            (ScalarValue::Decimal128(a, _, _), ScalarValue::Decimal128(b, _, _)) => {
                handle!(a, b)
            }
            (ScalarValue::Utf8(a), ScalarValue::Utf8(b)) => {
                if b.as_ref().unwrap() > a.as_ref().unwrap() {
                    *a = b;
                }
            }
            (ScalarValue::Date32(a), ScalarValue::Date32(b)) => handle!(a, b),
            (ScalarValue::Date64(a), ScalarValue::Date64(b)) => handle!(a, b),
            (other, _) => {
                return Err(DataFusionError::NotImplemented(format!(
                    "unsupported data type in max(): {}",
                    other
                )));
            }
        }
        Ok(())
    }
}