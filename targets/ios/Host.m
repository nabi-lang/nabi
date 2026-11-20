// The app that hosts the corpus tests (Corpus.m): it only has to launch.
// The case dylibs are in its Frameworks/ (xctest.sh). UIKit wants a
// scene, so it has one with an empty window.

#import <UIKit/UIKit.h>

@interface HostScene : UIResponder <UIWindowSceneDelegate>
@property(strong, nonatomic) UIWindow *window;
@end

@implementation HostScene
- (void)scene:(UIScene *)scene
	willConnectToSession:(UISceneSession *)session
		     options:(UISceneConnectionOptions *)options
{
	self.window = [[UIWindow alloc] initWithWindowScene:(UIWindowScene *)scene];
	self.window.rootViewController = [UIViewController new];
	[self.window makeKeyAndVisible];
}
@end

@interface HostDelegate : UIResponder <UIApplicationDelegate>
@end

@implementation HostDelegate
- (UISceneConfiguration *)application:(UIApplication *)app
	configurationForConnectingSceneSession:(UISceneSession *)session
				       options:(UISceneConnectionOptions *)options
{
	UISceneConfiguration *c = [UISceneConfiguration configurationWithName:@"Host"
								 sessionRole:session.role];
	c.delegateClass = HostScene.class;
	return c;
}
@end

int main(int argc, char *argv[])
{
	@autoreleasepool {
		return UIApplicationMain(argc, argv, nil, NSStringFromClass(HostDelegate.class));
	}
}
