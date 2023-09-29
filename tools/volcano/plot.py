#!/usr/bin/env python3
# Copyright 2023 Amazon.com, Inc. or its affiliates. All Rights Reserved.
# SPDX-License-Identifier: Apache-2.0
import math
import statistics

import plotext
from termcolor import colored

from . import TestResult

from host_tools.metrics import format_with_reduced_unit


def display_volcano_plot(results, *, relative=False):
    # Unit for all results should be the same
    unit = results[0].unit
    if relative:
        unit = "Percent"

    # All individual data points for all runs
    all_data_points_average = statistics.mean(
        [a for result in results for a in result.data_a]
        + [b for result in results for b in result.data_b]
    )

    # We want to plot the p-value as -log(p). Because of -log(p) = log(1/p), we get this
    # result by plotting 1/p on a log-scale axis.
    p_values = [1 / result.p_value for result in results]

    if relative:
        mean_differences = [
            result.mean_difference / statistics.mean(result.data_a)
            for result in results
        ]
    else:
        mean_differences = [result.mean_difference for result in results]

    print(
        "\nVolcano plot of recent A/B-Tests. Each point represents on test run. Total number of runs:",
        len(results),
    )
    if relative:
        print(
            f"The average reported regression is",
            colored(
                f"{statistics.mean(abs(x) for x in mean_differences):.2%}.",
                attrs=["bold"],
            ),
        )
    else:
        print(
            f"The average value across all runs so far is",
            colored(
                format_with_reduced_unit(all_data_points_average, unit), attrs=["bold"]
            )
            + ".",
        )

    print(
        "The sorted p-value/mean regressions (used in Bonferrati-Holm correction) are",
        sorted(result.p_value for result in results),
        "and",
        sorted(abs(result.mean_difference) for result in results),
    )

    plotext.clear_data()
    plotext.clear_figure()
    # We do our own log scaling logic on the x-axis below because plotext has no "symlog" equivalent
    plotext.yscale("log")
    plotext.xlabel(f"regression ({unit})")
    plotext.ylabel("-log(p value)")
    plotext.plot_size(70, 20)
    plotext.canvas_color("black")
    plotext.axes_color("black")
    plotext.ticks_color("white")

    plotext.yticks([1, 10, 100, 1000, 10000], [0, 1, 2, 3, 4])

    max_exp = math.ceil(
        math.log10(max(abs(diff) for diff in mean_differences if diff != 0))
    )
    min_exp = math.floor(
        math.log10(min(abs(diff) for diff in mean_differences if diff != 0))
    )
    translated_mean_differences = [
        diff * 10 ** (-min_exp + 1) for diff in mean_differences
    ]
    ticks = list(range(-(max_exp - min_exp) - 1, 0)) + list(
        range(0, max_exp - min_exp + 2)
    )
    tick_labels = (
        [f"-10^{i}" for i in range(min_exp, max_exp + 1)[::-1]]
        + [0]
        + [f"10^{i}" for i in range(min_exp, max_exp + 1)]
    )

    plotext.xticks(ticks, tick_labels)
    plotext.xlim(-(max_exp - min_exp) - 1, max_exp - min_exp + 1)
    plotext.ylim(0, 4)

    plotext.scatter(
        [
            math.log10(mean_diff) if mean_diff > 0 else -math.log10(-mean_diff)
            for mean_diff in translated_mean_differences
            if mean_diff != 0
        ],
        p_values,
    )
    plotext.hline(1 / 0.01, "red")
    if not relative:
        plotext.vline(math.log10(all_data_points_average * 0.2) - min_exp + 1, "red")
        plotext.vline(-(math.log10(all_data_points_average * 0.2) - min_exp + 1), "red")
    else:
        plotext.vline(math.log10(0.2) - min_exp + 1, "red")
        plotext.vline(-(math.log10(0.2) - min_exp + 1), "red")
    plotext.show()
    print()


def display_one_run(result: TestResult):
    a_mean = statistics.mean(result.data_a)

    print(
        f"See below the plot for buildkite build {result.build_number}. A/B-Testing determined that the p-value of observed change of",
        colored(
            format_with_reduced_unit(result.mean_difference, result.unit),
            attrs=["bold"],
        ),
        "from",
        colored(
            format_with_reduced_unit(a_mean, result.unit),
            attrs=["bold"],
        ),
        "to",
        colored(
            format_with_reduced_unit(statistics.mean(result.data_b), result.unit),
            attrs=["bold"],
        ),
        "or",
        colored(f"{result.mean_difference/a_mean:.2%}", attrs=["bold"]),
        "being a genuine performance change is",
        colored(str(result.p_value), attrs=["bold"]),
    )

    plotext.clear_data()
    plotext.clear_figure()
    plotext.plot_size(70, 20)
    plotext.canvas_color("black")
    plotext.axes_color("black")
    plotext.ticks_color("white")
    plotext.plot(result.data_a, label="A")
    plotext.plot(result.data_b, label="B")
    plotext.ylabel(f"{result.metric} ({result.unit})")
    plotext.show()
    print()


def display_histogram(results, *, regression=False):
    if regression:
        data = [result.relative_mean_difference for result in results]
    else:
        data = [result.p_value for result in results]

    plotext.clear_data()
    plotext.clear_figure()
    plotext.plot_size(150, 20)
    plotext.canvas_color("black")
    plotext.axes_color("black")
    plotext.ticks_color("white")
    plotext.hist(data, 200)
    if not regression:
        plotext.vline(0.01, "red")
    plotext.show()
