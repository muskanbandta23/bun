mod support;

use support::{
    date, date_string, date_time, date_time_string, duration, duration_string, zoned, zoned_string,
};

#[test]
fn round_trips() {
    assert_eq!(date_string(date("20240229")), "2024-02-29");
    assert_eq!(
        date_time_string(date_time("-000001-01-01 235960,5")),
        "-000001-01-01T23:59:59.5"
    );
    assert_eq!(
        zoned_string(&zoned("2024-03-10T02:30[!america/new_york][u-ca=hebrew]")),
        "2024-03-10T03:30:00-04:00[America/New_York][u-ca=hebrew]"
    );
    assert_eq!(duration_string(&duration("-p1y2dt1.5h")), "-P1Y2DT1H30M");
}
