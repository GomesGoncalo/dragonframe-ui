"""Tests of what the relay tells the frame.  Run: python3 -m unittest discover homeassistant/relay"""

import unittest

from relay import matters, named, report


def state(entity, value, name=None, kind=None):
    attributes = {"friendly_name": name} if name else {}
    if kind:
        attributes["device_class"] = kind
    return entity, {"entity_id": entity, "state": value, "attributes": attributes}


HOUSE = dict([
    state("alarm_control_panel.homebase", "armed_away", "Homebase"),
    state("alarm_control_panel.spare", "unknown", "Spare"),
    state("binary_sensor.front_door_sensor_open", "off", "Front Door Sensor open", "door"),
    state("binary_sensor.kitchen_window_sensor_open", "on", "Kitchen Window Sensor open", "door"),
    state("binary_sensor.office_sensor_open", "unavailable", "Office Sensor open", "door"),
    state("binary_sensor.kitchen_motion_detected", "on", "Kitchen Motion detected", "motion"),
    state("binary_sensor.doorbell_motion_detected", "on", "Doorbell Motion detected", "motion"),
    state("binary_sensor.doorbell_person_detected", "on", "Doorbell Person detected", "motion"),
    state("binary_sensor.doorbell_ringing", "off", "Doorbell Ringing", "running"),
    state("binary_sensor.front_door_debug_device", "off", "Front Door Debug (device)"),
    state("image.doorbell_event_image", "2026-10-13T12:00:00+00:00", "Doorbell Event Image"),
    state("image.side_passage_event_image", "2026-10-13T11:00:00+00:00", "Side Passage Event Image"),
    state("sensor.doorbell_battery", "80", "Doorbell Battery", "battery"),
])


class Reporting(unittest.TestCase):
    def test_the_house_is_put_in_the_frames_words(self):
        told = report(HOUSE)
        self.assertEqual(told["alarms"], [{"name": "Homebase", "state": "armed_away"}], "an alarm of unknown state is left out")
        self.assertEqual(told["sensors"], [
            {"name": "Front Door", "kind": "door", "on": False},
            {"name": "Kitchen", "kind": "motion", "on": True},
            {"name": "Kitchen Window", "kind": "window", "on": True},
        ], "unavailable sensors, debug sensors and a camera's own motion are left out")
        self.assertEqual(told["cameras"], [
            {"id": "doorbell", "name": "Doorbell", "person": True, "bell": True},
            {"id": "side_passage", "name": "Side Passage", "person": False, "bell": False},
        ])
        self.assertFalse(told["ringing"])

    def test_a_ring_is_passed_on(self):
        house = dict(HOUSE)
        house.update([state("binary_sensor.doorbell_ringing", "on", "Doorbell Ringing", "running")])
        self.assertTrue(report(house)["ringing"])

    def test_no_more_is_sent_than_the_frame_takes(self):
        many = dict(state(f"binary_sensor.d{n}_sensor_open", "off", f"Door {n} Sensor open", "door") for n in range(60))
        many.update(state(f"image.c{n}_event_image", "x", f"Cam {n} Event Image") for n in range(12))
        told = report(many)
        self.assertEqual((len(told["sensors"]), len(told["cameras"])), (40, 8))
        self.assertEqual(report({}), {"alarms": [], "sensors": [], "cameras": [], "ringing": False})

    def test_names_lose_what_home_assistant_adds(self):
        self.assertEqual(named({"entity_id": "x.y", "attributes": {"friendly_name": "Back Door Sensor open"}}, " Sensor open"), "Back Door")
        self.assertEqual(named({"entity_id": "x.y", "attributes": {}}), "x.y")
        self.assertEqual(len(named({"entity_id": "x.y", "attributes": {"friendly_name": "n" * 200}})), 60)

    def test_only_some_changes_are_worth_telling(self):
        self.assertTrue(matters("binary_sensor.front_door_sensor_open") and matters("image.doorbell_event_image"))
        self.assertTrue(matters("alarm_control_panel.homebase"))
        self.assertFalse(matters("sensor.doorbell_battery") or matters("sun.sun"))


if __name__ == "__main__":
    unittest.main()
