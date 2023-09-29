#!/usr/bin/env python3
# Copyright 2023 Amazon.com, Inc. or its affiliates. All Rights Reserved.
# SPDX-License-Identifier: Apache-2.0
import statistics
import sys
from functools import cached_property
from pathlib import Path
from typing import List

# Hack to be able to use our test framework code
sys.path.append(str(Path(__file__).parent.parent.parent / "tests"))

from framework.ab_test import check_regression


class TestResult:
    def __init__(
        self,
        data_a: List[float],
        data_b: List[float],
        p_value,
        mean_difference,
        build_number,
        unit,
        metric,
        *,
        resample_rate: int = None,
    ):
        self.data_a = data_a
        self.data_b = data_b
        self.build_number = build_number
        self.unit = unit
        self.metric = metric

        self._p_value = p_value
        self._mean_difference = mean_difference
        self._resample_rate = resample_rate

    @cached_property
    def _result(self):
        return check_regression(
            self.data_a, self.data_b, n_resamples=self._resample_rate
        )

    @property
    def p_value(self):
        if self._resample_rate is None:
            return self._p_value
        return self._result.pvalue

    @property
    def mean_difference(self):
        if self._resample_rate is None:
            return self._mean_difference
        return self._result.statistic

    @property
    def relative_mean_difference(self):
        return self.mean_difference / statistics.mean(self.data_a)

    def __repr__(self):
        items = ("%s = %r" % (k, v) for k, v in self.__dict__.items())
        return "<%s: {%s}>" % (self.__class__.__name__, ", ".join(items))
