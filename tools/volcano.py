#!/usr/bin/env python3
# Copyright 2023 Amazon.com, Inc. or its affiliates. All Rights Reserved.
# SPDX-License-Identifier: Apache-2.0
import argparse
import math
from pathlib import Path

import inquirer
import pandas
from termcolor import colored
from volcano import TestResult
from volcano.input import (
    eliminate_unchosen,
    inquire_dimension,
    maximum_entropy_dimension,
    prompt_for_buildkite_run,
    selection_in_loop,
)
from volcano.plot import display_histogram, display_one_run, display_volcano_plot


def print_metrics_analysis(df_row):
    print("Showing details for A/B-Tests performed with the following parameters:")
    for col in df_row.index:
        if col != 0:
            if not isinstance(df_row[col], float) or not math.isnan(df_row[col]):
                print(colored(f"{col:<20}", attrs=["bold"]), f"{df_row[col]}")
    print()

    options = [
        ("Display volcano plot of historical A/B-Tests", "volcano"),
        ("Display data for specific buildkite run", "buildkite"),
        ("Nothing, take me to next metric", "next"),
        ("Exit", "exit"),
    ]

    for choice in selection_in_loop(
        "What do you want to do with this metric?", options, default="next"
    ):
        match choice:
            case "volcano":
                display_volcano_plot(df_row[0])
            case "buildkite":
                try:
                    run_number = prompt_for_buildkite_run()
                except ValueError:
                    print("Please enter a valid integer")
                    continue

                if run_number is None:
                    continue

                try:
                    result = next(
                        result
                        for result in df_row[0]
                        if result.build_number == run_number
                    )
                except StopIteration:
                    print(f"No data for build number {run_number} found")
                else:
                    display_one_run(result)
            case "next":
                return True
            case "exit":
                return False


def print_holistic_analysis(df):
    print(
        "Performing holistic analysis of p-values logged by A/B-Tests matching the following dimensions:"
    )
    space = {
        dimension: list(set(df[dimension].dropna()))
        for dimension in df.columns
        if dimension != 0
    }

    for dimension, values in space.items():
        if len(values) != 1:
            continue

        print(colored(f"{dimension:<20}", attrs=["bold"]), f"{values[0]}")
    print("\nThis will include p-values across the following space:")
    for dimension, values in space.items():
        if len(values) <= 1:
            continue

        print(colored(f"{dimension:<20}", attrs=["bold"]), f"{values}")

    try:
        run_number = prompt_for_buildkite_run(
            message="Do you want to limit the analysis to a specific buildkite build? (for 'yes', provide build number, for 'no' leave empty)"
        )
    except ValueError:
        run_number = None

    results = [
        result
        for row in df[0]
        for result in row
        if run_number is None or result.build_number == run_number
    ]

    options = [
        ("Volcano plot of relative regressions", "volcano"),
        ("Histogram of p-values", "histogram-p"),
        ("Histogram of relative regressions", "histogram-r"),
        ("Exit", "exit"),
    ]

    for choice in selection_in_loop(
        "What type of aggregate plot are you interested in?", options, default="exit"
    ):
        match choice:
            case "volcano":
                display_volcano_plot(results, relative=True)
            case "histogram-p":
                display_histogram(results)
            case "histogram-r":
                display_histogram(results, regression=True)
            case "exit":
                return


if __name__ == "__main__":
    parser = argparse.ArgumentParser(
        description="Computes volcano plots for A/B-performance test results"
    )
    parser.add_argument(
        "emf_logs",
        help="Path to the ndjson file containing the A/B testing EMF logs",
        type=Path,
    )
    parser.add_argument(
        "--resample-rate",
        help="If provided, will re-run the permutation tests with the specified resample rate, giving more accurate p-values to work with. Note that should a high resample rate will result in (significant) delays when computing plots!",
        required=False,
        type=int,
    )
    args = parser.parse_args()

    df = pandas.read_json(args.emf_logs, lines=True)
    df = df[~df["metric"].isna()]
    aws = pandas.json_normalize(df["_aws"], "CloudWatchMetrics")
    dimensions = list(set(aws["Dimensions"].apply(lambda x: x[0]).explode()))

    grouped_results = (
        df.groupby(list(dimensions), dropna=False)[
            [
                "data_a",
                "data_b",
                "mean_difference",
                "p_value",
                "buildkite_build_number",
                "metric",
                "_aws",
            ]
        ]
        .apply(
            lambda x: [
                TestResult(*y, resample_rate=args.resample_rate)
                for y in zip(
                    x["data_a"],
                    x["data_b"],
                    x["p_value"],
                    x["mean_difference"],
                    x["buildkite_build_number"],
                    x["_aws"].apply(
                        lambda y: next(
                            z["Unit"]
                            for z in y["CloudWatchMetrics"][0]["Metrics"]
                            if z["Name"] == "mean_difference"
                        )
                    ),
                    x["metric"],
                )
            ]
        )
        .reset_index()
    )

    previous_choices = {}

    # Some dimensions are common to all test results, so prompt those first
    common_dimensions = [
        "performance_test",
        "instance",
        "guest_kernel",
        "host_kernel",
    ]

    def prompt_dimension(df, dimension):
        dimensions.remove(dimension)
        answer = inquire_dimension(df, dimension, previous_choices)

        if answer:
            previous_choices.update(**answer)
            return eliminate_unchosen(df, answer)

    for dim in common_dimensions:
        grouped_results = prompt_dimension(grouped_results, dim)

    while dimensions:
        max_entropy_dim = maximum_entropy_dimension(grouped_results, dimensions)

        if not max_entropy_dim:
            break

        grouped_results = prompt_dimension(grouped_results, max_entropy_dim)

    print()

    answer = inquirer.prompt(
        [
            inquirer.List(
                "action",
                message="What kind of investigation do you want to perform?",
                choices=[
                    (
                        "Holistic view of p-values distribution of selected metrics",
                        "holistic",
                    ),
                    ("One-by-one deep dive into each metric", "deep"),
                ],
            )
        ]
    )

    if answer:
        match answer["action"]:
            case "deep":
                for i in range(len(grouped_results)):
                    if not print_metrics_analysis(grouped_results.iloc[i]):
                        break
            case "holistic":
                print_holistic_analysis(grouped_results)
