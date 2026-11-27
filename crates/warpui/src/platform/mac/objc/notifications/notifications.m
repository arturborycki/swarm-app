#import "notifications.h"
#import "../app.h"

#import <UserNotifications/UserNotifications.h>

NSString *const WarpSwarmAllowAction = @"SWARM_ALLOW";

NSSet *warpNotificationCategories(void) {
    UNNotificationCategory *plain =
        [UNNotificationCategory categoryWithIdentifier:@"CUSTOMIZED_NOTIFICATION"
                                               actions:@[]
                                     intentIdentifiers:@[]
                                               options:UNNotificationCategoryOptionCustomDismissAction];
    // Answering doesn't bring the app forward; opening the session does.
    UNNotificationAction *allow = [UNNotificationAction actionWithIdentifier:WarpSwarmAllowAction
                                                                       title:@"Allow Once"
                                                                     options:UNNotificationActionOptionNone];
    UNNotificationAction *open = [UNNotificationAction actionWithIdentifier:@"SWARM_OPEN"
                                                                      title:@"Open"
                                                                    options:UNNotificationActionOptionForeground];
    UNNotificationCategory *permission =
        [UNNotificationCategory categoryWithIdentifier:@"SWARM_PERMISSION"
                                               actions:@[ allow, open ]
                                     intentIdentifiers:@[]
                                               options:UNNotificationCategoryOptionCustomDismissAction];
    return [NSSet setWithObjects:plain, permission, nil];
}

void requestNotificationPermissionsWithCompletionHandler(
    void (^completion_handler)(NSUInteger outcome_type, id outcome_msg)) {
    UNUserNotificationCenter *center = [UNUserNotificationCenter currentNotificationCenter];

    [center
        requestAuthorizationWithOptions:(UNAuthorizationOptionAlert + UNAuthorizationOptionSound +
                                         UNAuthorizationOptionBadge)
                      completionHandler:^(BOOL granted, NSError *_Nullable error) {
                        if (!granted) {
                            completion_handler(1, @"User denied request to receive notifications.");
                        } else if (error != nil) {
                            completion_handler(2, error.localizedDescription);
                        } else {
                            // Register the notification categories.
                            [center setNotificationCategories:warpNotificationCategories()];
                            completion_handler(0,
                                               @"User accepted request to receive notifications.");
                        }
                      }];
}

void requestNotificationPermissions(void *on_completion_callback) {
    requestNotificationPermissionsWithCompletionHandler(^(NSUInteger outcome_type, id outcome_msg) {
      dispatch_async(dispatch_get_main_queue(), ^{
        warp_on_request_notification_permissions_completed(outcome_type, outcome_msg,
                                                           on_completion_callback);
      });
    });
}

void sendNotificationWithErrorHandler(NSString *title, NSString *body, NSString *data,
                                      void (^error_handler)(NSUInteger error_type, id error_msg),
                                      BOOL playSound) {
    UNUserNotificationCenter *center = [UNUserNotificationCenter currentNotificationCenter];
    [center getNotificationSettingsWithCompletionHandler:^(UNNotificationSettings *settings) {
      if (settings.authorizationStatus == UNAuthorizationStatusDenied) {
          error_handler(0, @"User turned permissions off in system preferences.");
      } else {
          // Create the notification content.
          // `autorelease` balances the +1 retain from `alloc`; the enclosing UserNotifications
          // completion block runs on a GCD-dispatched queue that drains an ambient pool.
          UNMutableNotificationContent *content =
              [[[UNMutableNotificationContent alloc] init] autorelease];
          content.title = [NSString localizedUserNotificationStringForKey:title arguments:nil];
          content.body = [NSString localizedUserNotificationStringForKey:body arguments:nil];

          // Only play sound if the user setting allows it
          if (playSound) {
              content.sound = [UNNotificationSound defaultSound];
          }

          content.userInfo = @{
              @"DATA" : data,
          };
          // swarm-term: a session whose program waits for permission (the app
          // writes this into the data) gets the "Allow Once" button.
          if ([data containsString:@"\"permission\":true"]) {
              content.categoryIdentifier = @"SWARM_PERMISSION";
          }

          // Configure the trigger to send the notification after 1 second.
          UNTimeIntervalNotificationTrigger *trigger =
              [UNTimeIntervalNotificationTrigger triggerWithTimeInterval:1 repeats:NO];

          // Create the request object.
          UNNotificationRequest *request =
              [UNNotificationRequest requestWithIdentifier:@"CUSTOMIZED_NOTIFICATION"
                                                   content:content
                                                   trigger:trigger];

          // Schedule the notification.
          [center addNotificationRequest:request
                   withCompletionHandler:^(NSError *_Nullable err) {
                     if (err != nil) {
                         error_handler(1, err.localizedDescription);
                     }
                   }];
      }
    }];
}

void sendNotification(id title, id body, id data, void *on_error_callback, BOOL playSound) {
    sendNotificationWithErrorHandler(
        title, body, data,
        ^(NSUInteger error_type, id error_msg) {
          dispatch_async(dispatch_get_main_queue(), ^{
            warp_on_notification_send_error(error_type, error_msg, on_error_callback);
          });
        },
        playSound);
}
